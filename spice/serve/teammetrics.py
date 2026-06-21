"""Agent-sourced lane metric storage and summaries."""

from __future__ import annotations

import json
import os
import sqlite3
import time
from collections import Counter
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Iterable, Mapping, Protocol

from spice.errors import SpiceError
from spice.serve.directivestats import DirectiveTotals
from spice.serve.teamfilters import shell_settings_from_json
from spice.serve.teamschema import (
    DEFAULT_STUCK_THRESHOLD_SECONDS,
    METRIC_HISTORY_RETENTION_SECONDS,
)

METRIC_BUCKET_SECONDS = 60
METRIC_HISTORY_RETENTION_DAYS_ENV = (
    "SPICE_METRIC_HISTORY_RETENTION_DAYS"  # env-policy: allow
)
TASK_EVENT_KINDS = frozenset({"claim", "phaseAdvance", "review", "complete", "drain"})
_SECONDS_PER_DAY = 24 * 60 * 60


@dataclass(frozen=True)
class LaneMetricSummary:
    agent_ids: tuple[str, ...]
    acked: int
    sends: int
    tool_calls: int
    sparkline: tuple[int, ...]


@dataclass(frozen=True)
class MetricSeriesPoint:
    bucket_start: int
    messages: int


@dataclass(frozen=True)
class TaskLifecycleSeriesPoint:
    bucket_start: int
    claimed: int
    active: int
    completed: int
    drained: int


@dataclass(frozen=True)
class TaskDistributionSeriesPoint:
    bucket_start: int
    agent_id: str
    claimed: int
    active: int
    share: float


@dataclass(frozen=True)
class TaskStallState:
    task_id: str
    agent_id: str
    team_id: str
    claimed_at: float
    last_activity_at: float
    last_progress_at: float
    idle_seconds: int
    threshold_seconds: int
    stuck: bool


@dataclass(frozen=True)
class TeamHistoricalMetricSummary:
    team_id: str
    agent_ids: tuple[str, ...]
    messages: int
    sparkline: tuple[int, ...]


@dataclass(frozen=True)
class _MembershipInterval:
    team_id: str
    agent_id: str
    start: float
    end: float


@dataclass(frozen=True)
class _ActiveTaskClaim:
    task_id: str
    agent_id: str
    team_id: str
    claimed_at: float


class _TeamMetricStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def current_team_for_agent(self, agent_id: str) -> str | None: ...

    def _record_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        agent_id: str,
        *,
        team_id: str,
        tool_calls: int,
        message_buckets: Counter[int],
        tool_call_buckets: Counter[int],
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
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> LaneMetricSummary: ...

    def _directive_totals_for_agents_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: Iterable[str],
        *,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> DirectiveTotals: ...

    def _prune_directive_history_locked(
        self,
        connection: sqlite3.Connection,
        *,
        now: float,
        retention_seconds: int = METRIC_HISTORY_RETENTION_SECONDS,
    ) -> None: ...


class TeamMetricStoreMixin:
    def record_agent_metric_delta(
        self: _TeamMetricStore,
        agent_id: str,
        *,
        tool_calls: int = 0,
        message_timestamps: Iterable[float] = (),
        tool_call_timestamps: Iterable[float] = (),
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        tool_calls = _nonnegative_int(tool_calls)
        now = time.time()
        message_buckets = Counter(
            _metric_bucket_start(timestamp) for timestamp in message_timestamps
        )
        tool_call_buckets = Counter(
            _metric_bucket_start(timestamp) for timestamp in tool_call_timestamps
        )
        recorded_tool_calls = sum(tool_call_buckets.values())
        if recorded_tool_calls > tool_calls:
            raise SpiceError("tool_call_timestamps cannot exceed tool_calls")
        if tool_calls > recorded_tool_calls:
            tool_call_buckets[_metric_bucket_start(now)] += (
                tool_calls - recorded_tool_calls
            )
        if tool_calls == 0 and not message_buckets:
            return
        # Tag the activity with the team the agent is on at capture time, or the
        # agent itself when it is in no team / a private solo team.
        team_id = self.current_team_for_agent(agent_id) or agent_id
        with self.connect() as connection:
            self._record_agent_metric_delta_locked(
                connection,
                agent_id,
                team_id=team_id,
                tool_calls=tool_calls,
                message_buckets=message_buckets,
                tool_call_buckets=tool_call_buckets,
                now=now,
            )

    def agent_metric_cursor(
        self: _TeamMetricStore, agent_id: str, source_path: str
    ) -> int:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            row = connection.execute(
                "SELECT offset FROM agent_metric_cursors "
                "WHERE agent_id = ? AND source_path = ?",
                (agent_id, source_path),
            ).fetchone()
        if row is None:
            return 0
        return max(0, int(row["offset"] or 0))

    def metric_history_retention_seconds(self: _TeamMetricStore) -> int:
        with self.connect() as connection:
            return _metric_history_retention_seconds_locked(connection)

    def record_agent_metric_cursor(
        self: _TeamMetricStore, agent_id: str, *, source_path: str, offset: int
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            connection.execute(
                "INSERT INTO agent_metric_cursors "
                "(agent_id, source_path, offset, updated_at) VALUES (?, ?, ?, ?) "
                "ON CONFLICT(agent_id, source_path) DO UPDATE SET "
                "offset = excluded.offset, "
                "updated_at = excluded.updated_at",
                (agent_id, source_path, max(0, int(offset)), time.time()),
            )

    def record_task_lifecycle_event(
        self: _TeamMetricStore,
        kind: str,
        *,
        task_id: str,
        agent_id: str,
        team_id: str | None = None,
        ts: float | None = None,
    ) -> None:
        kind = _task_event_kind(kind)
        task_id = _normalized_id(task_id, "task_id")
        agent_id = _normalized_id(agent_id, "agent_id")
        capture_team_id = (
            _normalized_id(team_id, "team_id")
            if team_id is not None
            else self.current_team_for_agent(agent_id) or agent_id
        )
        event_time = time.time() if ts is None else max(0.0, float(ts))
        with self.connect() as connection:
            connection.execute(
                "INSERT INTO task_events "
                "(ts, kind, task_id, agent_id, team_id) VALUES (?, ?, ?, ?, ?)",
                (event_time, kind, task_id, agent_id, capture_team_id),
            )

    def _rewrite_agent_metric_cursors_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
    ) -> None:
        old_agent_id = _normalized_id(old_agent_id, "old_agent_id")
        new_agent_id = _normalized_id(new_agent_id, "new_agent_id")
        if old_agent_id == new_agent_id:
            return
        connection.execute(
            "INSERT INTO agent_metric_cursors "
            "(agent_id, source_path, offset, updated_at) "
            "SELECT ?, source_path, offset, updated_at "
            "FROM agent_metric_cursors WHERE agent_id = ? "
            "ON CONFLICT(agent_id, source_path) DO UPDATE SET "
            "offset = max(agent_metric_cursors.offset, excluded.offset), "
            "updated_at = max(agent_metric_cursors.updated_at, excluded.updated_at)",
            (new_agent_id, old_agent_id),
        )
        connection.execute(
            "DELETE FROM agent_metric_cursors WHERE agent_id = ?", (old_agent_id,)
        )

    def _rewrite_agent_metrics_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
    ) -> None:
        # Renewal unifies the predecessor's id into the successor (the canonical
        # actor), so the predecessor's per-agent counters fold into the successor
        # and only one id survives. This is what makes lineage accumulate under
        # the membership-derived read; see serve-team-metric-attribution.md (D9).
        old_agent_id = _normalized_id(old_agent_id, "old_agent_id")
        new_agent_id = _normalized_id(new_agent_id, "new_agent_id")
        if old_agent_id == new_agent_id:
            return
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, tool_calls, updated_at) "
            "SELECT ?, team_id, tool_calls, updated_at "
            "FROM agent_metrics WHERE agent_id = ? "
            "ON CONFLICT(agent_id, team_id) DO UPDATE SET "
            "tool_calls = agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = max(agent_metrics.updated_at, excluded.updated_at)",
            (new_agent_id, old_agent_id),
        )
        connection.execute(
            "INSERT INTO agent_metric_buckets "
            "(agent_id, team_id, bucket_start, messages, tool_calls) "
            "SELECT ?, team_id, bucket_start, messages, tool_calls "
            "FROM agent_metric_buckets WHERE agent_id = ? "
            "ON CONFLICT(agent_id, team_id, bucket_start) DO UPDATE SET "
            "messages = agent_metric_buckets.messages + excluded.messages, "
            "tool_calls = agent_metric_buckets.tool_calls + excluded.tool_calls",
            (new_agent_id, old_agent_id),
        )
        connection.execute(
            "DELETE FROM agent_metrics WHERE agent_id = ?", (old_agent_id,)
        )
        connection.execute(
            "DELETE FROM agent_metric_buckets WHERE agent_id = ?", (old_agent_id,)
        )

    def _rewrite_task_lifecycle_events_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
    ) -> None:
        old_agent_id = _normalized_id(old_agent_id, "old_agent_id")
        new_agent_id = _normalized_id(new_agent_id, "new_agent_id")
        if old_agent_id == new_agent_id:
            return
        connection.execute(
            "UPDATE task_events SET agent_id = ? WHERE agent_id = ?",
            (new_agent_id, old_agent_id),
        )

    def lane_metric_summary(
        self: _TeamMetricStore,
        agent_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
        now: float | None = None,
        since_latest_renewal: bool = False,
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
                    "ORDER BY position",
                    (str(row["team_id"]),),
                ).fetchall()
                member_ids = tuple(str(member["agent_id"]) for member in member_rows)
            else:
                member_ids = (agent_id,)
            start_time_by_agent = (
                _latest_renewal_start_times_locked(connection, member_ids)
                if since_latest_renewal
                else None
            )
            return self._agent_lane_metric_summary_locked(
                connection,
                member_ids,
                bucket_count=bucket_count,
                bucket_seconds=bucket_seconds,
                now=summary_time,
                start_time_by_agent=start_time_by_agent,
            )

    def team_historical_metric_summary(
        self: _TeamMetricStore,
        team_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
        now: float | None = None,
    ) -> TeamHistoricalMetricSummary:
        team_id = _normalized_id(team_id, "team_id")
        bucket_count = max(1, int(bucket_count))
        bucket_seconds = max(1, int(bucket_seconds))
        summary_time = time.time() if now is None else max(0.0, float(now))
        with self.connect() as connection:
            intervals = [
                interval
                for interval in _membership_intervals_from_events(
                    connection, end_time=summary_time
                )
                if interval.team_id == team_id
            ]
            agent_ids = _historical_agent_ids(intervals)
            buckets = _historical_metric_buckets(connection, intervals, agent_ids)
        return TeamHistoricalMetricSummary(
            team_id=team_id,
            agent_ids=agent_ids,
            messages=sum(buckets.values()),
            sparkline=_metric_sparkline(
                buckets.items(),
                bucket_count=bucket_count,
                bucket_seconds=bucket_seconds,
                now=summary_time,
            ),
        )

    def agent_activity_series(
        self: _TeamMetricStore,
        agent_ids: Iterable[str],
        *,
        start: float,
        end: float,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
    ) -> tuple[MetricSeriesPoint, ...]:
        """Stable, full-fidelity activity series for graphing: summed messages
        per bucket over the given agents within [start, end]. Unlike the lane
        sparkline this applies no rolling window or aging — re-querying the same
        range always yields identical points, so it can be plotted over an
        arbitrary range (bounded only by the retention horizon)."""
        ids = tuple(dict.fromkeys(str(agent_id) for agent_id in agent_ids if agent_id))
        if not ids:
            return ()
        bucket_seconds = max(1, int(bucket_seconds))
        floor = _metric_bucket_start(start, bucket_seconds)
        ceiling = _metric_bucket_start(end, bucket_seconds)
        placeholders = ",".join("?" for _ in ids)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT bucket_start, SUM(messages) AS messages "
                "FROM agent_metric_buckets "
                f"WHERE agent_id IN ({placeholders}) "
                "AND bucket_start >= ? AND bucket_start <= ? "
                "GROUP BY bucket_start ORDER BY bucket_start",
                (*ids, floor, ceiling),
            ).fetchall()
        return tuple(
            MetricSeriesPoint(int(row["bucket_start"]), int(row["messages"] or 0))
            for row in rows
        )

    def task_lifecycle_series(
        self: _TeamMetricStore,
        agent_ids: Iterable[str] = (),
        *,
        team_ids: Iterable[str] = (),
        start: float,
        end: float,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
    ) -> tuple[TaskLifecycleSeriesPoint, ...]:
        """Stable task-flow series for graphing: task lifecycle facts folded
        into per-bucket movement counts. The substrate is append-only
        task_events tagged with actor and team-at-capture, so re-querying the
        same range yields the same projection until retention prunes it."""
        agents = _normalized_ids(agent_ids, "agent_id")
        teams = _normalized_ids(team_ids, "team_id")
        if not agents and not teams:
            return ()
        bucket_seconds = max(1, int(bucket_seconds))
        start_time = max(0.0, float(start))
        end_time = max(start_time, float(end))
        bucket_expr = (
            f"(CAST(ts AS INTEGER) - (CAST(ts AS INTEGER) % {bucket_seconds}))"
        )
        filters = ["ts >= ?", "ts <= ?"]
        params: list[object] = [start_time, end_time]
        if agents:
            filters.append(f"agent_id IN ({_placeholders(agents)})")
            params.extend(agents)
        if teams:
            filters.append(f"team_id IN ({_placeholders(teams)})")
            params.extend(teams)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT "
                f"{bucket_expr} AS bucket_start, "
                "SUM(CASE WHEN kind = 'claim' THEN 1 ELSE 0 END) AS claimed, "
                "SUM(CASE WHEN kind IN ('phaseAdvance', 'review') "
                "THEN 1 ELSE 0 END) AS active, "
                "SUM(CASE WHEN kind = 'complete' THEN 1 ELSE 0 END) AS completed, "
                "SUM(CASE WHEN kind = 'drain' THEN 1 ELSE 0 END) AS drained "
                "FROM task_events "
                f"WHERE {' AND '.join(filters)} "
                "GROUP BY bucket_start ORDER BY bucket_start",
                params,
            ).fetchall()
        return tuple(
            TaskLifecycleSeriesPoint(
                bucket_start=int(row["bucket_start"]),
                claimed=int(row["claimed"] or 0),
                active=int(row["active"] or 0),
                completed=int(row["completed"] or 0),
                drained=int(row["drained"] or 0),
            )
            for row in rows
        )

    def task_distribution_series(
        self: _TeamMetricStore,
        agent_ids: Iterable[str] = (),
        *,
        team_ids: Iterable[str] = (),
        start: float,
        end: float,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
    ) -> tuple[TaskDistributionSeriesPoint, ...]:
        """Per-agent share of claimed/active task-flow movement by bucket."""
        agents = _normalized_ids(agent_ids, "agent_id")
        teams = _normalized_ids(team_ids, "team_id")
        if not agents and not teams:
            return ()
        bucket_seconds = max(1, int(bucket_seconds))
        start_time = max(0.0, float(start))
        end_time = max(start_time, float(end))
        bucket_expr = (
            f"(CAST(ts AS INTEGER) - (CAST(ts AS INTEGER) % {bucket_seconds}))"
        )
        filters = [
            "ts >= ?",
            "ts <= ?",
            "kind IN ('claim', 'phaseAdvance', 'review')",
        ]
        params: list[object] = [start_time, end_time]
        if agents:
            filters.append(f"agent_id IN ({_placeholders(agents)})")
            params.extend(agents)
        if teams:
            filters.append(f"team_id IN ({_placeholders(teams)})")
            params.extend(teams)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT "
                f"{bucket_expr} AS bucket_start, "
                "agent_id, "
                "SUM(CASE WHEN kind = 'claim' THEN 1 ELSE 0 END) AS claimed, "
                "SUM(CASE WHEN kind IN ('phaseAdvance', 'review') "
                "THEN 1 ELSE 0 END) AS active "
                "FROM task_events "
                f"WHERE {' AND '.join(filters)} "
                "GROUP BY bucket_start, agent_id ORDER BY bucket_start, agent_id",
                params,
            ).fetchall()
        totals_by_bucket: Counter[int] = Counter()
        parsed_rows: list[tuple[int, str, int, int]] = []
        for row in rows:
            bucket_start = int(row["bucket_start"])
            agent_id = str(row["agent_id"])
            claimed = int(row["claimed"] or 0)
            active = int(row["active"] or 0)
            work = claimed + active
            if work <= 0:
                continue
            parsed_rows.append((bucket_start, agent_id, claimed, active))
            totals_by_bucket[bucket_start] += work
        return tuple(
            TaskDistributionSeriesPoint(
                bucket_start=bucket_start,
                agent_id=agent_id,
                claimed=claimed,
                active=active,
                share=(claimed + active) / totals_by_bucket[bucket_start],
            )
            for bucket_start, agent_id, claimed, active in parsed_rows
        )

    def task_stall_states(
        self: _TeamMetricStore,
        agent_ids: Iterable[str] = (),
        *,
        team_ids: Iterable[str] = (),
        now: float | None = None,
        threshold_seconds: int = DEFAULT_STUCK_THRESHOLD_SECONDS,
    ) -> tuple[TaskStallState, ...]:
        """Current stuck/stall projection over task lifecycle facts.

        A task is a candidate while its latest lifecycle fact is a claim. The
        stall timer starts at that claim and is reset by later agent activity
        buckets; a phase advance, review completion, or drain removes the task
        from the active set because the latest lifecycle fact is no longer a
        claim.
        """
        agents = _normalized_ids(agent_ids, "agent_id")
        teams = _normalized_ids(team_ids, "team_id")
        sample_time = time.time() if now is None else max(0.0, float(now))
        threshold = max(1, int(threshold_seconds))
        with self.connect() as connection:
            claims = _active_task_claims_locked(
                connection, agent_ids=agents, team_ids=teams
            )
            activity_by_agent = _activity_bucket_times_by_agent_locked(
                connection, claims
            )
        return tuple(
            _task_stall_state(
                claim,
                activity_by_agent.get(claim.agent_id, ()),
                now=sample_time,
                threshold_seconds=threshold,
            )
            for claim in claims
        )

    def _prune_metric_history_locked(
        self: _TeamMetricStore, connection: sqlite3.Connection, *, now: float
    ) -> None:
        # Bound the high-growth per-minute bucket and per-directive series at the
        # retention horizon; the durable aggregates (agent_metrics tool_calls,
        # directive_totals) are never pruned. Runs in the snapshot prune pass.
        retention_seconds = _metric_history_retention_seconds_locked(connection)
        floor = int(now) - retention_seconds
        connection.execute(
            "DELETE FROM agent_metric_buckets WHERE bucket_start < ?", (floor,)
        )
        connection.execute("DELETE FROM task_events WHERE ts < ?", (float(floor),))
        self._prune_directive_history_locked(
            connection, now=now, retention_seconds=retention_seconds
        )

    def _record_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        agent_id: str,
        *,
        team_id: str,
        tool_calls: int,
        message_buckets: Counter[int],
        tool_call_buckets: Counter[int],
        now: float,
    ) -> None:
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, tool_calls, updated_at) "
            "VALUES (?, ?, ?, ?) "
            "ON CONFLICT(agent_id, team_id) DO UPDATE SET "
            "tool_calls = agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = excluded.updated_at",
            (agent_id, team_id, tool_calls, now),
        )
        bucket_starts = sorted(set(message_buckets) | set(tool_call_buckets))
        for bucket_start in bucket_starts:
            connection.execute(
                "INSERT INTO agent_metric_buckets "
                "(agent_id, team_id, bucket_start, messages, tool_calls) "
                "VALUES (?, ?, ?, ?, ?) "
                "ON CONFLICT(agent_id, team_id, bucket_start) DO UPDATE SET "
                "messages = agent_metric_buckets.messages + excluded.messages, "
                "tool_calls = agent_metric_buckets.tool_calls + excluded.tool_calls",
                (
                    agent_id,
                    team_id,
                    bucket_start,
                    int(message_buckets.get(bucket_start, 0)),
                    int(tool_call_buckets.get(bucket_start, 0)),
                ),
            )

    def _agent_lane_metric_summary_locked(
        self: _TeamMetricStore,
        connection: sqlite3.Connection,
        agent_ids: tuple[str, ...],
        *,
        bucket_count: int,
        bucket_seconds: int,
        now: float,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> LaneMetricSummary:
        if not agent_ids:
            return LaneMetricSummary(
                agent_ids=(),
                acked=0,
                sends=0,
                tool_calls=0,
                sparkline=tuple(0 for _ in range(max(0, bucket_count))),
            )
        start_times = _metric_start_times(agent_ids, start_time_by_agent)
        # sends/acked are the membership-derived directive totals (acked <= sends
        # by construction); tool_calls is the per-agent activity counter.
        directives = self._directive_totals_for_agents_locked(
            connection, agent_ids, start_time_by_agent=start_times
        )
        lifetime_tool_calls = _lifetime_tool_calls_locked(connection, agent_ids)
        # Only buckets inside the sparkline window contribute, so bound the read
        # there instead of scanning the agent's whole (unbounded) bucket history
        # on every render. Mirror _metric_sparkline's window start exactly.
        window_floor = _metric_bucket_start(now, bucket_seconds) - (
            (bucket_count - 1) * bucket_seconds
        )
        message_buckets, window_tool_calls = _lane_activity_buckets_locked(
            connection,
            agent_ids,
            window_floor=window_floor,
            start_time_by_agent=start_times,
        )
        return _lane_metric_summary_from_buckets(
            agent_ids,
            message_buckets.items(),
            acked=directives.acked,
            sends=directives.sends,
            tool_calls=window_tool_calls if start_times else lifetime_tool_calls,
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
            now=now,
        )


def _normalized_id(value: str, field_name: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise SpiceError(f"{field_name} must be non-empty")
    return normalized


def _metric_history_retention_seconds_locked(connection: sqlite3.Connection) -> int:
    rows = connection.execute(
        "SELECT shell_settings FROM teams WHERE status = 'open' "
        "ORDER BY created_at, team_id"
    ).fetchall()
    for row in rows:
        configured = _retention_seconds_from_settings(
            shell_settings_from_json(row["shell_settings"])
        )
        if configured is not None:
            return configured
    env_value = os.environ.get(METRIC_HISTORY_RETENTION_DAYS_ENV, "").strip()
    if env_value:
        return _positive_days_seconds(env_value, METRIC_HISTORY_RETENTION_DAYS_ENV)
    return METRIC_HISTORY_RETENTION_SECONDS


def _retention_seconds_from_settings(settings: dict[str, object]) -> int | None:
    metrics = settings.get("metrics")
    if metrics is not None and not isinstance(metrics, dict):
        raise SpiceError("shellSettings.metrics must be an object")
    metric_settings = metrics if isinstance(metrics, dict) else {}
    if "historyRetentionSeconds" in metric_settings:
        return _positive_seconds(
            metric_settings["historyRetentionSeconds"],
            "shellSettings.metrics.historyRetentionSeconds",
        )
    if "historyRetentionDays" in metric_settings:
        return _positive_days_seconds(
            metric_settings["historyRetentionDays"],
            "shellSettings.metrics.historyRetentionDays",
        )
    if "retentionDays" in metric_settings:
        return _positive_days_seconds(
            metric_settings["retentionDays"],
            "shellSettings.metrics.retentionDays",
        )
    if "metricHistoryRetentionDays" in settings:
        return _positive_days_seconds(
            settings["metricHistoryRetentionDays"],
            "shellSettings.metricHistoryRetentionDays",
        )
    return None


def _positive_days_seconds(value: object, field_name: str) -> int:
    try:
        days = float(str(value))
    except (TypeError, ValueError) as exc:
        raise SpiceError(f"{field_name} must be a positive number") from exc
    if days <= 0:
        raise SpiceError(f"{field_name} must be positive")
    return max(1, int(days * _SECONDS_PER_DAY))


def _positive_seconds(value: object, field_name: str) -> int:
    try:
        seconds = int(str(value))
    except (TypeError, ValueError) as exc:
        raise SpiceError(f"{field_name} must be a positive integer") from exc
    if seconds <= 0:
        raise SpiceError(f"{field_name} must be positive")
    return seconds


def _normalized_ids(values: Iterable[str], field_name: str) -> tuple[str, ...]:
    return tuple(
        dict.fromkeys(
            _normalized_id(value, field_name)
            for value in values
            if str(value or "").strip()
        )
    )


def _task_event_kind(value: str) -> str:
    kind = str(value or "").strip()
    if kind not in TASK_EVENT_KINDS:
        allowed = ", ".join(sorted(TASK_EVENT_KINDS))
        raise SpiceError(f"task event kind must be one of {allowed}: {kind!r}")
    return kind


def _placeholders(values: tuple[str, ...]) -> str:
    return ",".join("?" for _value in values)


def _active_task_claims_locked(
    connection: sqlite3.Connection,
    *,
    agent_ids: tuple[str, ...],
    team_ids: tuple[str, ...],
) -> tuple[_ActiveTaskClaim, ...]:
    filters = ["kind = 'claim'"]
    params: list[object] = []
    if agent_ids:
        filters.append(f"agent_id IN ({_placeholders(agent_ids)})")
        params.extend(agent_ids)
    if team_ids:
        filters.append(f"team_id IN ({_placeholders(team_ids)})")
        params.extend(team_ids)
    rows = connection.execute(
        "WITH latest AS ("
        "  SELECT task_events.rowid, task_events.ts, task_events.kind, "
        "         task_events.task_id, task_events.agent_id, task_events.team_id "
        "  FROM task_events "
        "  JOIN ("
        "    SELECT task_id, MAX(rowid) AS rowid "
        "    FROM task_events GROUP BY task_id"
        "  ) AS latest_event ON task_events.rowid = latest_event.rowid"
        ") "
        "SELECT task_id, agent_id, team_id, ts FROM latest "
        f"WHERE {' AND '.join(filters)} "
        "ORDER BY ts, task_id",
        params,
    ).fetchall()
    return tuple(
        _ActiveTaskClaim(
            task_id=str(row["task_id"]),
            agent_id=str(row["agent_id"]),
            team_id=str(row["team_id"]),
            claimed_at=float(row["ts"] or 0.0),
        )
        for row in rows
    )


def _activity_bucket_times_by_agent_locked(
    connection: sqlite3.Connection,
    claims: tuple[_ActiveTaskClaim, ...],
) -> dict[str, tuple[float, ...]]:
    if not claims:
        return {}
    agent_ids = tuple(dict.fromkeys(claim.agent_id for claim in claims))
    query_floor = min(_metric_bucket_start(claim.claimed_at) for claim in claims)
    rows = connection.execute(
        "SELECT agent_id, bucket_start FROM agent_metric_buckets "
        f"WHERE agent_id IN ({_placeholders(agent_ids)}) "
        "AND bucket_start >= ? "
        "AND (messages > 0 OR tool_calls > 0) "
        "ORDER BY bucket_start",
        (*agent_ids, query_floor),
    ).fetchall()
    by_agent: dict[str, list[float]] = {}
    for row in rows:
        by_agent.setdefault(str(row["agent_id"]), []).append(
            float(row["bucket_start"] or 0.0)
        )
    return {agent_id: tuple(times) for agent_id, times in by_agent.items()}


def _task_stall_state(
    claim: _ActiveTaskClaim,
    activity_times: tuple[float, ...],
    *,
    now: float,
    threshold_seconds: int,
) -> TaskStallState:
    activity_floor = _metric_bucket_start(claim.claimed_at)
    last_activity = max(
        (timestamp for timestamp in activity_times if timestamp >= activity_floor),
        default=0.0,
    )
    last_progress = max(claim.claimed_at, last_activity)
    idle_seconds = max(0, int(now - last_progress))
    return TaskStallState(
        task_id=claim.task_id,
        agent_id=claim.agent_id,
        team_id=claim.team_id,
        claimed_at=claim.claimed_at,
        last_activity_at=last_activity,
        last_progress_at=last_progress,
        idle_seconds=idle_seconds,
        threshold_seconds=threshold_seconds,
        stuck=idle_seconds >= threshold_seconds,
    )


def _nonnegative_int(value: int) -> int:
    return max(0, int(value or 0))


def _membership_intervals_from_events(
    connection: sqlite3.Connection, *, end_time: float
) -> list[_MembershipInterval]:
    open_memberships: dict[str, tuple[str, float]] = {}
    intervals: list[_MembershipInterval] = []
    rows = connection.execute(
        "SELECT ts, kind, team_id, payload FROM events ORDER BY revision"
    ).fetchall()
    for row in rows:
        timestamp = float(row["ts"] or 0.0)
        if timestamp > end_time:
            continue
        team_id = str(row["team_id"] or "")
        kind = str(row["kind"] or "")
        payload = _event_payload(row)
        if kind == "createTeam":
            for agent_id in _event_agent_ids(payload, "members"):
                _move_membership(
                    open_memberships, intervals, agent_id, team_id, timestamp
                )
        elif kind == "assignAgent":
            _move_membership(
                open_memberships,
                intervals,
                _event_agent_id(payload, "agentId"),
                team_id,
                timestamp,
            )
        elif kind == "removeAgent":
            _close_membership(
                open_memberships,
                intervals,
                _event_agent_id(payload, "agentId"),
                team_id,
                timestamp,
            )
        elif kind == "closeTeam":
            _close_team_memberships(open_memberships, intervals, team_id, timestamp)
        elif kind == "mergeTeams":
            source_team_id = _event_team_id(payload, "sourceTeamId")
            for agent_id in _event_agent_ids(payload, "agents"):
                _move_membership_from_team(
                    open_memberships,
                    intervals,
                    agent_id,
                    source_team_id,
                    team_id,
                    timestamp,
                )
        elif kind == "splitTeam":
            new_team_id = _event_team_id(payload, "newTeamId")
            for agent_id in _event_agent_ids(payload, "agents"):
                _move_membership_from_team(
                    open_memberships,
                    intervals,
                    agent_id,
                    team_id,
                    new_team_id,
                    timestamp,
                )
        elif kind == "splitTeamBack":
            restored_team_id = _event_team_id(payload, "restoredTeamId")
            for agent_id in _event_agent_ids(payload, "agents"):
                _move_membership_from_team(
                    open_memberships,
                    intervals,
                    agent_id,
                    team_id,
                    restored_team_id,
                    timestamp,
                )
    for agent_id, (team_id, start) in open_memberships.items():
        intervals.append(
            _MembershipInterval(
                team_id=team_id,
                agent_id=agent_id,
                start=start,
                end=end_time,
            )
        )
    return intervals


def _event_payload(row: sqlite3.Row) -> dict[str, object]:
    payload = json.loads(str(row["payload"] or "{}"))
    if not isinstance(payload, dict):
        raise SpiceError("team event payload must be a JSON object")
    return payload


def _event_agent_id(payload: dict[str, object], key: str) -> str:
    value = payload.get(key)
    if not isinstance(value, str) or not value:
        raise SpiceError(f"team event payload {key} must be a non-empty string")
    return value


def _event_team_id(payload: dict[str, object], key: str) -> str:
    value = payload.get(key)
    if not isinstance(value, str) or not value:
        raise SpiceError(f"team event payload {key} must be a non-empty string")
    return value


def _event_agent_ids(payload: dict[str, object], key: str) -> list[str]:
    value = payload.get(key)
    if not isinstance(value, list) or not all(
        isinstance(agent_id, str) and agent_id for agent_id in value
    ):
        raise SpiceError(f"team event payload {key} must be a list of agent ids")
    return [str(agent_id) for agent_id in value]


def _move_membership(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[_MembershipInterval],
    agent_id: str,
    team_id: str,
    timestamp: float,
) -> None:
    current = open_memberships.pop(agent_id, None)
    if current is not None:
        current_team_id, started_at = current
        intervals.append(
            _MembershipInterval(
                team_id=current_team_id,
                agent_id=agent_id,
                start=started_at,
                end=timestamp,
            )
        )
    open_memberships[agent_id] = (team_id, timestamp)


def _move_membership_from_team(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[_MembershipInterval],
    agent_id: str,
    source_team_id: str,
    destination_team_id: str,
    timestamp: float,
) -> None:
    _close_membership(open_memberships, intervals, agent_id, source_team_id, timestamp)
    open_memberships[agent_id] = (destination_team_id, timestamp)


def _close_membership(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[_MembershipInterval],
    agent_id: str,
    team_id: str,
    timestamp: float,
) -> None:
    current = open_memberships.pop(agent_id, None)
    if current is None or current[0] != team_id:
        raise SpiceError(
            f"cannot reconstruct team metric interval for {agent_id} in {team_id}"
        )
    intervals.append(
        _MembershipInterval(
            team_id=team_id,
            agent_id=agent_id,
            start=current[1],
            end=timestamp,
        )
    )


def _close_team_memberships(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[_MembershipInterval],
    team_id: str,
    timestamp: float,
) -> None:
    for agent_id, (current_team_id, started_at) in tuple(open_memberships.items()):
        if current_team_id != team_id:
            continue
        intervals.append(
            _MembershipInterval(
                team_id=team_id,
                agent_id=agent_id,
                start=started_at,
                end=timestamp,
            )
        )
        del open_memberships[agent_id]


def _historical_agent_ids(
    intervals: list[_MembershipInterval],
) -> tuple[str, ...]:
    ordered = sorted(
        intervals, key=lambda interval: (interval.start, interval.agent_id)
    )
    return tuple(dict.fromkeys(interval.agent_id for interval in ordered))


def _historical_metric_buckets(
    connection: sqlite3.Connection,
    intervals: list[_MembershipInterval],
    agent_ids: tuple[str, ...],
) -> Counter[int]:
    if not agent_ids:
        return Counter()
    intervals_by_agent: dict[str, list[_MembershipInterval]] = {}
    for interval in intervals:
        intervals_by_agent.setdefault(interval.agent_id, []).append(interval)
    placeholders = ",".join("?" for _agent_id in agent_ids)
    rows = connection.execute(
        "SELECT agent_id, bucket_start, messages FROM agent_metric_buckets "
        f"WHERE agent_id IN ({placeholders}) ORDER BY bucket_start",
        agent_ids,
    ).fetchall()
    buckets: Counter[int] = Counter()
    for row in rows:
        agent_id = str(row["agent_id"])
        bucket_start = int(row["bucket_start"])
        messages = int(row["messages"] or 0)
        if any(
            interval.start <= bucket_start < interval.end
            for interval in intervals_by_agent[agent_id]
        ):
            buckets[bucket_start] += messages
    return buckets


def _latest_renewal_start_times_locked(
    connection: sqlite3.Connection,
    agent_ids: tuple[str, ...],
) -> dict[str, float]:
    wanted = set(agent_ids)
    if not wanted:
        return {}
    rows = connection.execute(
        "SELECT ts, payload FROM events WHERE kind = 'renewalStarted' ORDER BY revision"
    ).fetchall()
    start_times: dict[str, float] = {}
    for row in rows:
        payload = _event_payload(row)
        successor = _event_agent_id(payload, "successor")
        if successor not in wanted:
            continue
        start_times[successor] = max(
            start_times.get(successor, 0.0),
            float(row["ts"] or 0.0),
        )
    return start_times


def _metric_start_times(
    agent_ids: tuple[str, ...],
    start_time_by_agent: Mapping[str, float] | None,
) -> dict[str, float]:
    if not start_time_by_agent:
        return {}
    return {
        agent_id: max(0.0, float(start_time_by_agent[agent_id]))
        for agent_id in agent_ids
        if agent_id in start_time_by_agent
    }


def _lifetime_tool_calls_locked(
    connection: sqlite3.Connection,
    agent_ids: tuple[str, ...],
) -> int:
    placeholders = ",".join("?" for _ in agent_ids)
    row = connection.execute(
        "SELECT COALESCE(SUM(tool_calls), 0) AS tool_calls "
        f"FROM agent_metrics WHERE agent_id IN ({placeholders})",
        agent_ids,
    ).fetchone()
    return int(row["tool_calls"] or 0) if row else 0


def _lane_activity_buckets_locked(
    connection: sqlite3.Connection,
    agent_ids: tuple[str, ...],
    *,
    window_floor: int,
    start_time_by_agent: Mapping[str, float],
) -> tuple[Counter[int], int]:
    if not start_time_by_agent:
        return _lifetime_lane_activity_buckets_locked(
            connection, agent_ids, window_floor=window_floor
        )
    placeholders = ",".join("?" for _ in agent_ids)
    earliest_start = min(
        start_time_by_agent.get(agent_id, 0.0) for agent_id in agent_ids
    )
    query_floor = min(window_floor, int(earliest_start))
    rows = connection.execute(
        "SELECT agent_id, bucket_start, messages, tool_calls "
        "FROM agent_metric_buckets "
        f"WHERE agent_id IN ({placeholders}) AND bucket_start >= ? "
        "ORDER BY bucket_start",
        (*agent_ids, query_floor),
    ).fetchall()
    message_buckets: Counter[int] = Counter()
    tool_calls = 0
    for row in rows:
        agent_id = str(row["agent_id"])
        bucket_start = int(row["bucket_start"])
        if bucket_start < start_time_by_agent.get(agent_id, 0.0):
            continue
        if bucket_start >= window_floor:
            message_buckets[bucket_start] += int(row["messages"] or 0)
        tool_calls += int(row["tool_calls"] or 0)
    return message_buckets, tool_calls


def _lifetime_lane_activity_buckets_locked(
    connection: sqlite3.Connection,
    agent_ids: tuple[str, ...],
    *,
    window_floor: int,
) -> tuple[Counter[int], int]:
    placeholders = ",".join("?" for _ in agent_ids)
    rows = connection.execute(
        "SELECT bucket_start, SUM(messages) AS messages, "
        "SUM(tool_calls) AS tool_calls "
        "FROM agent_metric_buckets "
        f"WHERE agent_id IN ({placeholders}) AND bucket_start >= ? "
        "GROUP BY bucket_start ORDER BY bucket_start",
        (*agent_ids, window_floor),
    ).fetchall()
    message_buckets: Counter[int] = Counter()
    tool_calls = 0
    for row in rows:
        message_buckets[int(row["bucket_start"])] += int(row["messages"] or 0)
        tool_calls += int(row["tool_calls"] or 0)
    return message_buckets, tool_calls


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


def _lane_metric_summary_from_buckets(
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
        sparkline=_metric_sparkline(
            ((int(bucket), int(count)) for bucket, count in bucket_rows),
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
            now=now,
        ),
    )
