"""Historical metric buckets, lane summaries, and sparkline rendering.

Bucket construction and rendering are pure folds over rows the store's
locked accessors already read: attributing message buckets to membership
intervals, ordering historical agents, and rendering sparklines and lane
summary payloads. No SQL runs here -- database ownership stays with the
store mixin.
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from enum import StrEnum
from typing import Iterable

from spice.serve.team.membership import MembershipInterval, StoreRow

METRIC_BUCKET_SECONDS = 60
# Cap high-growth historical metric payloads before callers allocate sparkline
# or series buckets for accidental unbounded ranges.
TEAM_HISTORICAL_MAX_BUCKET_COUNT = 1440


class ObservationAttributionMode(StrEnum):
    """The actor/team lens applied to immutable observation facts."""

    SOURCE_ACTOR = "sourceActor"
    LINEAGE_CUMULATIVE = "lineageCumulative"
    PER_SESSION = "perSession"
    TEAM_AT_EVENT_TIME = "teamAtEventTime"


@dataclass(frozen=True)
class LaneMetricSummary:
    agent_ids: tuple[str, ...]
    acked: int
    sends: int
    tool_calls: int
    sparkline: tuple[int, ...]


@dataclass(frozen=True)
class TeamHistoricalMetricSummary:
    team_id: str
    agent_ids: tuple[str, ...]
    messages: int
    sparkline: tuple[int, ...]


def metric_bucket_start(
    timestamp: float, bucket_seconds: int = METRIC_BUCKET_SECONDS
) -> int:
    raw = max(0, int(float(timestamp)))
    bucket_seconds = max(1, int(bucket_seconds))
    return raw - (raw % bucket_seconds)


def metric_sparkline(
    rows: Iterable[tuple[int, int]],
    *,
    bucket_count: int,
    bucket_seconds: int,
    now: float,
) -> tuple[int, ...]:
    values = [0] * bucket_count
    bucket_rows = [(bucket, count) for bucket, count in rows if count > 0]
    if not bucket_rows:
        return tuple(values)
    latest = metric_bucket_start(now, bucket_seconds)
    start = latest - ((bucket_count - 1) * bucket_seconds)
    for bucket, count in bucket_rows:
        index = (bucket - start) // bucket_seconds
        if index < 0:
            continue
        values[min(index, bucket_count - 1)] += count
    return tuple(values)


def empty_lane_metric_summary(
    bucket_count: int, agent_ids: tuple[str, ...] = ()
) -> LaneMetricSummary:
    return LaneMetricSummary(
        agent_ids=agent_ids,
        acked=0,
        sends=0,
        tool_calls=0,
        sparkline=tuple(0 for _ in range(max(0, bucket_count))),
    )


def lane_metric_summary_from_buckets(
    agent_ids: tuple[str, ...],
    bucket_rows: Iterable[tuple[int, int]],
    *,
    acked: int,
    sends: int,
    tool_calls: int,
    bucket_count: int,
    bucket_seconds: int,
    now: float,
) -> LaneMetricSummary:
    return LaneMetricSummary(
        agent_ids=agent_ids,
        acked=acked,
        sends=sends,
        tool_calls=tool_calls,
        sparkline=metric_sparkline(
            ((int(bucket), int(count)) for bucket, count in bucket_rows),
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
            now=now,
        ),
    )


def historical_agent_ids(
    intervals: list[MembershipInterval],
) -> tuple[str, ...]:
    ordered = sorted(
        intervals, key=lambda interval: (interval.start, interval.agent_id)
    )
    return tuple(dict.fromkeys(interval.agent_id for interval in ordered))


def historical_metric_buckets(
    rows: Iterable[StoreRow],
    intervals: list[MembershipInterval],
) -> Counter[int]:
    """Fold per-agent message bucket rows into team-attributed buckets.

    A bucket counts only while its agent was a member: interval starts are
    inclusive and interval ends exclusive, so activity in the departure
    minute belongs to the next team.
    """
    intervals_by_agent: dict[str, list[MembershipInterval]] = {}
    for interval in intervals:
        intervals_by_agent.setdefault(interval.agent_id, []).append(interval)
    buckets: Counter[int] = Counter()
    for row in rows:
        agent_id = str(row["agent_id"])
        bucket_start = int(row["bucket_start"])
        messages = int(row["messages"] or 0)
        if any(
            interval.start <= bucket_start < interval.end
            for interval in intervals_by_agent.get(agent_id, ())
        ):
            buckets[bucket_start] += messages
    return buckets
