"""Team-scoped lane metric storage and summaries."""

from __future__ import annotations

import sqlite3
import time
from collections import Counter
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Iterable, Protocol

from spice.errors import SpiceError

METRIC_BUCKET_SECONDS = 60


@dataclass(frozen=True)
class LaneMetricSummary:
    agent_ids: tuple[str, ...]
    acked: int
    sends: int
    tool_calls: int
    sparkline: tuple[int, ...]


class _TeamMetricStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def _record_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        agent_id: str,
        *,
        acked: int,
        sends: int,
        tool_calls: int,
        buckets: Counter[int],
        now: float,
    ) -> None: ...

    def _agent_lane_metric_summary_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: tuple[str, ...],
        *,
        bucket_count: int,
        bucket_seconds: int,
        now: float,
    ) -> LaneMetricSummary: ...


class TeamMetricStoreMixin:
    def record_agent_metric_delta(
        self: _TeamMetricStore,
        agent_id: str,
        *,
        acked: int = 0,
        sends: int = 0,
        tool_calls: int = 0,
        message_timestamps: Iterable[float] = (),
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        acked = _nonnegative_int(acked)
        sends = _nonnegative_int(sends)
        tool_calls = _nonnegative_int(tool_calls)
        buckets = Counter(
            _metric_bucket_start(timestamp) for timestamp in message_timestamps
        )
        if acked == sends == tool_calls == 0 and not buckets:
            return
        now = time.time()
        with self.connect() as connection:
            self._record_agent_metric_delta_locked(
                connection,
                agent_id,
                acked=acked,
                sends=sends,
                tool_calls=tool_calls,
                buckets=buckets,
                now=now,
            )

    def agent_metric_cursor(
        self: _TeamMetricStore, agent_id: str, source_path: str
    ) -> int:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            row = connection.execute(
                "SELECT source_path, offset FROM agent_metric_cursors "
                "WHERE agent_id = ?",
                (agent_id,),
            ).fetchone()
        if row is None or str(row["source_path"]) != source_path:
            return 0
        return max(0, int(row["offset"] or 0))

    def record_agent_metric_cursor(
        self: _TeamMetricStore, agent_id: str, *, source_path: str, offset: int
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            connection.execute(
                "INSERT INTO agent_metric_cursors "
                "(agent_id, source_path, offset, updated_at) VALUES (?, ?, ?, ?) "
                "ON CONFLICT(agent_id) DO UPDATE SET "
                "source_path = excluded.source_path, "
                "offset = excluded.offset, "
                "updated_at = excluded.updated_at",
                (agent_id, source_path, max(0, int(offset)), time.time()),
            )

    def lane_metric_summary(
        self: _TeamMetricStore,
        agent_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
        now: float | None = None,
    ) -> LaneMetricSummary:
        if not str(agent_id or "").strip():
            return LaneMetricSummary(
                agent_ids=(),
                acked=0,
                sends=0,
                tool_calls=0,
                sparkline=tuple(0 for _ in range(max(0, bucket_count))),
            )
        agent_id = _normalized_id(agent_id, "agent_id")
        bucket_count = max(1, int(bucket_count))
        bucket_seconds = max(1, int(bucket_seconds))
        summary_time = time.time() if now is None else max(0.0, float(now))
        with self.connect() as connection:
            # Derive the lane summary from CURRENT membership: the metric is the
            # aggregate of the team's current members' per-agent counters, so work
            # follows the agent across moves rather than staying bolted to a team.
            # See docs/studies/serve-team-metric-attribution.md (D3, D4).
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
            ).fetchone()
            if row is not None:
                member_rows = connection.execute(
                    "SELECT agent_id FROM memberships WHERE team_id = ? "
                    "ORDER BY joined_at",
                    (str(row["team_id"]),),
                ).fetchall()
                member_ids = tuple(str(member["agent_id"]) for member in member_rows)
            else:
                member_ids = (agent_id,)
            return self._agent_lane_metric_summary_locked(
                connection,
                member_ids,
                bucket_count=bucket_count,
                bucket_seconds=bucket_seconds,
                now=summary_time,
            )

    def _move_team_metric_rows_locked(
        self,
        connection: sqlite3.Connection,
        source_team_id: str,
        destination_team_id: str,
    ) -> None:
        connection.execute(
            "INSERT INTO team_agent_metrics "
            "(team_id, agent_id, acked, sends, tool_calls, updated_at) "
            "SELECT ?, agent_id, acked, sends, tool_calls, updated_at "
            "FROM team_agent_metrics WHERE team_id = ? "
            "ON CONFLICT(team_id, agent_id) DO UPDATE SET "
            "acked = team_agent_metrics.acked + excluded.acked, "
            "sends = team_agent_metrics.sends + excluded.sends, "
            "tool_calls = team_agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = max(team_agent_metrics.updated_at, excluded.updated_at)",
            (destination_team_id, source_team_id),
        )
        connection.execute(
            "INSERT INTO team_agent_metric_buckets "
            "(team_id, agent_id, bucket_start, messages) "
            "SELECT ?, agent_id, bucket_start, messages "
            "FROM team_agent_metric_buckets WHERE team_id = ? "
            "ON CONFLICT(team_id, agent_id, bucket_start) DO UPDATE SET "
            "messages = team_agent_metric_buckets.messages + excluded.messages",
            (destination_team_id, source_team_id),
        )
        connection.execute(
            "DELETE FROM team_agent_metrics WHERE team_id = ?", (source_team_id,)
        )
        connection.execute(
            "DELETE FROM team_agent_metric_buckets WHERE team_id = ?",
            (source_team_id,),
        )

    def _move_team_metric_rows_for_agents_locked(
        self,
        connection: sqlite3.Connection,
        source_team_id: str,
        destination_team_id: str,
        agent_ids: Iterable[str],
    ) -> None:
        agent_list = tuple(dict.fromkeys(str(agent_id) for agent_id in agent_ids))
        if not agent_list:
            return
        placeholders = ",".join("?" for _agent_id in agent_list)
        params = (destination_team_id, source_team_id, *agent_list)
        connection.execute(
            "INSERT INTO team_agent_metrics "
            "(team_id, agent_id, acked, sends, tool_calls, updated_at) "
            "SELECT ?, agent_id, acked, sends, tool_calls, updated_at "
            "FROM team_agent_metrics "
            f"WHERE team_id = ? AND agent_id IN ({placeholders}) "
            "ON CONFLICT(team_id, agent_id) DO UPDATE SET "
            "acked = team_agent_metrics.acked + excluded.acked, "
            "sends = team_agent_metrics.sends + excluded.sends, "
            "tool_calls = team_agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = max(team_agent_metrics.updated_at, excluded.updated_at)",
            params,
        )
        connection.execute(
            "INSERT INTO team_agent_metric_buckets "
            "(team_id, agent_id, bucket_start, messages) "
            "SELECT ?, agent_id, bucket_start, messages "
            "FROM team_agent_metric_buckets "
            f"WHERE team_id = ? AND agent_id IN ({placeholders}) "
            "ON CONFLICT(team_id, agent_id, bucket_start) DO UPDATE SET "
            "messages = team_agent_metric_buckets.messages + excluded.messages",
            params,
        )
        delete_params = (source_team_id, *agent_list)
        connection.execute(
            "DELETE FROM team_agent_metrics "
            f"WHERE team_id = ? AND agent_id IN ({placeholders})",
            delete_params,
        )
        connection.execute(
            "DELETE FROM team_agent_metric_buckets "
            f"WHERE team_id = ? AND agent_id IN ({placeholders})",
            delete_params,
        )

    def _record_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        agent_id: str,
        *,
        acked: int,
        sends: int,
        tool_calls: int,
        buckets: Counter[int],
        now: float,
    ) -> None:
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, acked, sends, tool_calls, updated_at) "
            "VALUES (?, ?, ?, ?, ?) "
            "ON CONFLICT(agent_id) DO UPDATE SET "
            "acked = agent_metrics.acked + excluded.acked, "
            "sends = agent_metrics.sends + excluded.sends, "
            "tool_calls = agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = excluded.updated_at",
            (agent_id, acked, sends, tool_calls, now),
        )
        for bucket_start, count in buckets.items():
            connection.execute(
                "INSERT INTO agent_metric_buckets "
                "(agent_id, bucket_start, messages) VALUES (?, ?, ?) "
                "ON CONFLICT(agent_id, bucket_start) DO UPDATE SET "
                "messages = agent_metric_buckets.messages + excluded.messages",
                (agent_id, bucket_start, int(count)),
            )

    def _agent_lane_metric_summary_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: tuple[str, ...],
        *,
        bucket_count: int,
        bucket_seconds: int,
        now: float,
    ) -> LaneMetricSummary:
        placeholders = ",".join("?" for _ in agent_ids)
        totals = connection.execute(
            "SELECT COALESCE(SUM(acked), 0) AS acked, "
            "COALESCE(SUM(sends), 0) AS sends, "
            "COALESCE(SUM(tool_calls), 0) AS tool_calls "
            f"FROM agent_metrics WHERE agent_id IN ({placeholders})",
            agent_ids,
        ).fetchone()
        bucket_rows = connection.execute(
            "SELECT bucket_start, SUM(messages) AS messages "
            "FROM agent_metric_buckets "
            f"WHERE agent_id IN ({placeholders}) "
            "GROUP BY bucket_start ORDER BY bucket_start",
            agent_ids,
        ).fetchall()
        return _lane_metric_summary_from_rows(
            agent_ids,
            totals,
            bucket_rows,
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
            now=now,
        )


def _normalized_id(value: str, field_name: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise SpiceError(f"{field_name} must be non-empty")
    return normalized


def _nonnegative_int(value: int) -> int:
    return max(0, int(value or 0))


def _metric_bucket_start(
    timestamp: float, bucket_seconds: int = METRIC_BUCKET_SECONDS
) -> int:
    raw = max(0, int(float(timestamp)))
    bucket_seconds = max(1, int(bucket_seconds))
    return raw - (raw % bucket_seconds)


def _metric_sparkline(
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
    latest = _metric_bucket_start(now, bucket_seconds)
    start = latest - ((bucket_count - 1) * bucket_seconds)
    for bucket, count in bucket_rows:
        index = (bucket - start) // bucket_seconds
        if index < 0:
            continue
        values[min(index, bucket_count - 1)] += count
    return tuple(values)


def _lane_metric_summary_from_rows(
    agent_ids: tuple[str, ...],
    totals: sqlite3.Row | None,
    bucket_rows: Iterable[sqlite3.Row],
    *,
    bucket_count: int,
    bucket_seconds: int,
    now: float,
) -> LaneMetricSummary:
    return LaneMetricSummary(
        agent_ids=agent_ids,
        acked=int(totals["acked"] or 0) if totals else 0,
        sends=int(totals["sends"] or 0) if totals else 0,
        tool_calls=int(totals["tool_calls"] or 0) if totals else 0,
        sparkline=_metric_sparkline(
            (
                (int(row["bucket_start"]), int(row["messages"] or 0))
                for row in bucket_rows
            ),
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
            now=now,
        ),
    )
