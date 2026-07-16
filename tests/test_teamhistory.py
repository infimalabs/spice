"""Historical bucket construction, lane summaries, and sparkline rendering."""

from __future__ import annotations

from collections import Counter

from spice.serve.team.history import (
    METRIC_BUCKET_SECONDS,
    LaneMetricSummary,
    empty_lane_metric_summary,
    historical_agent_ids,
    historical_metric_buckets,
    lane_metric_summary_from_buckets,
    metric_bucket_start,
    metric_sparkline,
)
from spice.serve.team.membership import MembershipInterval

BUCKET_COUNT = 4
NOW = 600.0
WINDOW_START = 420
PRE_WINDOW_BUCKET = 300
FUTURE_BUCKET = 900
INTERVAL_START = 120.0
INTERVAL_END = 240.0
FRACTIONAL_TS = 125.9
FRACTIONAL_TS_FLOOR = 120
COARSE_BUCKET_SECONDS = 100
COARSE_FLOOR = 100


def test_metric_bucket_start_floors_to_bucket_edges():
    assert metric_bucket_start(FRACTIONAL_TS) == FRACTIONAL_TS_FLOOR
    assert metric_bucket_start(FRACTIONAL_TS, COARSE_BUCKET_SECONDS) == COARSE_FLOOR
    assert metric_bucket_start(-5.0) == 0
    assert metric_bucket_start(NOW) == int(NOW)


def test_metric_sparkline_places_buckets_and_clamps_edges():
    rows = [
        (PRE_WINDOW_BUCKET, 7),
        (WINDOW_START, 1),
        (WINDOW_START + METRIC_BUCKET_SECONDS, 2),
        (FUTURE_BUCKET, 5),
    ]
    sparkline = metric_sparkline(
        rows,
        bucket_count=BUCKET_COUNT,
        bucket_seconds=METRIC_BUCKET_SECONDS,
        now=NOW,
    )
    assert sparkline == (1, 2, 0, 5)


def test_metric_sparkline_renders_zeroes_without_rows():
    sparkline = metric_sparkline(
        [],
        bucket_count=BUCKET_COUNT,
        bucket_seconds=METRIC_BUCKET_SECONDS,
        now=NOW,
    )
    assert sparkline == (0, 0, 0, 0)


def test_empty_lane_metric_summary_zeroes_every_field():
    summary = empty_lane_metric_summary(BUCKET_COUNT, agent_ids=("agent-1",))
    assert summary == LaneMetricSummary(
        agent_ids=("agent-1",),
        acked=0,
        sends=0,
        tool_calls=0,
        sparkline=(0, 0, 0, 0),
    )


def test_lane_metric_summary_from_buckets_renders_stable_values():
    summary = lane_metric_summary_from_buckets(
        ("agent-1",),
        [(WINDOW_START, 3), (WINDOW_START + METRIC_BUCKET_SECONDS, 4)],
        acked=2,
        sends=5,
        tool_calls=9,
        bucket_count=BUCKET_COUNT,
        bucket_seconds=METRIC_BUCKET_SECONDS,
        now=NOW,
    )
    assert summary == LaneMetricSummary(
        agent_ids=("agent-1",),
        acked=2,
        sends=5,
        tool_calls=9,
        sparkline=(3, 4, 0, 0),
    )


def test_historical_agent_ids_order_by_first_start_and_dedupe():
    intervals = [
        MembershipInterval(
            team_id="team-a",
            agent_id="agent-late",
            start=INTERVAL_END,
            end=NOW,
        ),
        MembershipInterval(
            team_id="team-a",
            agent_id="agent-early",
            start=INTERVAL_START,
            end=INTERVAL_END,
        ),
        MembershipInterval(
            team_id="team-b",
            agent_id="agent-early",
            start=INTERVAL_END,
            end=NOW,
        ),
    ]
    assert historical_agent_ids(intervals) == ("agent-early", "agent-late")


def test_historical_metric_buckets_use_inclusive_start_exclusive_end():
    intervals = [
        MembershipInterval(
            team_id="team-a",
            agent_id="agent-1",
            start=INTERVAL_START,
            end=INTERVAL_END,
        ),
    ]
    rows = [
        {"agent_id": "agent-1", "bucket_start": int(INTERVAL_START), "messages": 3},
        {"agent_id": "agent-1", "bucket_start": int(INTERVAL_END), "messages": 5},
        {"agent_id": "agent-other", "bucket_start": int(INTERVAL_START), "messages": 7},
    ]
    buckets = historical_metric_buckets(rows, intervals)
    assert buckets == Counter({int(INTERVAL_START): 3})


def test_historical_metric_buckets_render_empty_history_as_empty_counter():
    assert historical_metric_buckets([], []) == Counter()
