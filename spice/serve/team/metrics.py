"""Agent-sourced lane metric storage: the locked SQL ownership boundary.

The mixin owns the database: every SQL statement for lane metrics lives here
as a locked accessor. The semantics layered on top live with their seams --
membership-interval reconstruction in spice.serve.team.membership, and bucket
construction, lane summaries, and sparkline rendering in
spice.serve.team.history.
"""

from __future__ import annotations

import math
import os
import sqlite3
import time
from collections import Counter
from contextlib import AbstractContextManager
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Protocol

from spice.errors import SpiceError
from spice.serve.directivestats import DirectiveTotals
from spice.serve.team.filters import shell_settings_from_json
from spice.serve.team.history import (
    METRIC_BUCKET_SECONDS,
    LaneMetricSummary,
    ObservationAttributionMode,
    TeamHistoricalMetricSummary,
    empty_lane_metric_summary,
    historical_agent_ids,
    historical_metric_buckets,
    lane_metric_summary_from_buckets,
    metric_bucket_start,
    metric_sparkline,
)
from spice.serve.team.membership import (
    event_agent_id,
    event_payload,
    membership_intervals_from_events,
)
from spice.serve.team.schema import (
    DEFAULT_STUCK_THRESHOLD_SECONDS,
    METRIC_HISTORY_RETENTION_SECONDS,
    OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED,
)
from spice.transcript.reader import TranscriptFileIdentity

METRIC_HISTORY_RETENTION_DAYS_ENV = (
    "SPICE_METRIC_HISTORY_RETENTION_DAYS"  # env-policy: allow
)
TASK_EVENT_KINDS = frozenset({"claim", "phaseAdvance", "review", "complete", "drain"})
_SECONDS_PER_DAY = 24 * 60 * 60
OBSERVATION_SOURCE_REBUILD_REQUIRED = (
    "immutable source attribution is unavailable for pre-transition observation "
    "rows; rebuild Serve observation projections from their native facts"
)


@dataclass(frozen=True)
class AgentMetricCheckpoint:
    """Where one agent's ingestion of one transcript resumes, and from which file.

    `file_identity` is absent only before the first successful read of a source,
    which the reader treats as "no replacement claim to check" rather than as a
    replacement.
    """

    source_path: str
    offset: int
    file_identity: TranscriptFileIdentity | None


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
        source_path: str,
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


class TeamMetricStoreMixin:
    def observation_actor_ids(
        self: _TeamMetricStore,
        agent_ids: Iterable[str],
        *,
        attribution: ObservationAttributionMode,
    ) -> tuple[str, ...]:
        requested = _normalized_ids(agent_ids, "agent_id")
        if not requested:
            return ()
        with self.connect() as connection:
            if attribution is ObservationAttributionMode.SOURCE_ACTOR:
                return requested
            if attribution is ObservationAttributionMode.LINEAGE_CUMULATIVE:
                return _lineage_actor_ids_locked(connection, requested)
            if attribution is ObservationAttributionMode.PER_SESSION:
                return requested
        raise SpiceError(
            "teamAtEventTime attribution uses immutable team-at-event provenance"
        )

    def record_agent_metric_delta(
        self: _TeamMetricStore,
        agent_id: str,
        *,
        tool_calls: int = 0,
        message_timestamps: Iterable[float] = (),
        tool_call_timestamps: Iterable[float] = (),
        checkpoint: AgentMetricCheckpoint | None = None,
    ) -> None:
        """Count one batch of lane activity, with the checkpoint that produced it.

        Facts read out of a transcript arrive with the resume point they were
        read up to, and the two commit together: a checkpoint that landed
        without its facts skips activity forever, and facts that landed without
        their checkpoint are counted again on the next pass.
        """
        agent_id = _normalized_id(agent_id, "agent_id")
        tool_calls = _nonnegative_int(tool_calls)
        now = time.time()
        message_buckets = Counter(
            metric_bucket_start(timestamp) for timestamp in message_timestamps
        )
        tool_call_buckets = Counter(
            metric_bucket_start(timestamp) for timestamp in tool_call_timestamps
        )
        recorded_tool_calls = sum(tool_call_buckets.values())
        if recorded_tool_calls > tool_calls:
            raise SpiceError("tool_call_timestamps cannot exceed tool_calls")
        if tool_calls > recorded_tool_calls:
            tool_call_buckets[metric_bucket_start(now)] += (
                tool_calls - recorded_tool_calls
            )
        counted = tool_calls > 0 or bool(message_buckets)
        if not counted and checkpoint is None:
            return
        # Tag the activity with the team the agent is on at capture time, or the
        # agent itself when it is in no team / a private solo team.
        team_id = self.current_team_for_agent(agent_id) or agent_id
        # Counts are attributed to the source that produced them, so a replay of
        # one source reverses only its own contribution. Activity counted
        # outside a transcript pass has no source to replay from.
        source_path = "" if checkpoint is None else checkpoint.source_path
        with self.connect() as connection:
            if checkpoint is not None:
                _reset_unaccountable_metrics_locked(
                    connection, agent_id, source_path=source_path
                )
            if counted:
                self._record_agent_metric_delta_locked(
                    connection,
                    agent_id,
                    team_id=team_id,
                    source_path=source_path,
                    tool_calls=tool_calls,
                    message_buckets=message_buckets,
                    tool_call_buckets=tool_call_buckets,
                    now=now,
                )
            if checkpoint is not None:
                _record_agent_metric_cursor_locked(
                    connection, agent_id, checkpoint=checkpoint, now=now
                )

    def agent_metric_checkpoint(
        self: _TeamMetricStore, agent_id: str, source_path: str
    ) -> AgentMetricCheckpoint:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            row = connection.execute(
                "SELECT offset, source_device, source_inode FROM agent_metric_cursors "
                "WHERE agent_id = ? AND source_path = ?",
                (agent_id, source_path),
            ).fetchone()
        if row is None:
            return AgentMetricCheckpoint(
                source_path=source_path, offset=0, file_identity=None
            )
        return AgentMetricCheckpoint(
            source_path=source_path,
            offset=max(0, int(row["offset"] or 0)),
            file_identity=_file_identity(row["source_device"], row["source_inode"]),
        )

    def metric_history_retention_seconds(self: _TeamMetricStore) -> int:
        with self.connect() as connection:
            return _metric_history_retention_seconds_locked(connection)

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

    def _inherit_agent_metric_cursors_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
    ) -> None:
        old_agent_id = _normalized_id(old_agent_id, "old_agent_id")
        new_agent_id = _normalized_id(new_agent_id, "new_agent_id")
        if old_agent_id == new_agent_id:
            return
        # A successor inherits the predecessor's resume point for a source it
        # keeps reading, identity included: the file it resumes is the same file,
        # so an inherited checkpoint must not read as a replacement.
        connection.execute(
            "INSERT INTO agent_metric_cursors "
            "(agent_id, source_path, offset, source_device, source_inode, "
            "updated_at) "
            "SELECT ?, source_path, offset, source_device, source_inode, updated_at "
            "FROM agent_metric_cursors WHERE agent_id = ? "
            "ON CONFLICT(agent_id, source_path) DO UPDATE SET "
            "offset = max(agent_metric_cursors.offset, excluded.offset), "
            "source_device = CASE WHEN excluded.offset > agent_metric_cursors.offset "
            "THEN excluded.source_device ELSE agent_metric_cursors.source_device END, "
            "source_inode = CASE WHEN excluded.offset > agent_metric_cursors.offset "
            "THEN excluded.source_inode ELSE agent_metric_cursors.source_inode END, "
            "updated_at = max(agent_metric_cursors.updated_at, excluded.updated_at)",
            (new_agent_id, old_agent_id),
        )

    def lane_metric_summary(
        self: _TeamMetricStore,
        agent_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
        now: float | None = None,
        attribution: ObservationAttributionMode = (
            ObservationAttributionMode.LINEAGE_CUMULATIVE
        ),
    ) -> LaneMetricSummary:
        if not str(agent_id or "").strip():
            return empty_lane_metric_summary(bucket_count)
        agent_id = _normalized_id(agent_id, "agent_id")
        bucket_count = max(1, int(bucket_count))
        bucket_seconds = max(1, int(bucket_seconds))
        summary_time = time.time() if now is None else max(0.0, float(now))
        with self.connect() as connection:
            # Derive the lane summary from CURRENT membership: the metric is the
            # aggregate of the team's current members' per-agent counters, so work
            # follows the agent across moves rather than staying bolted to a team.
            # See docs/design/accepted/serve-team-metric-attribution.md (D3, D4).
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
            if attribution is ObservationAttributionMode.SOURCE_ACTOR:
                _require_source_attribution_locked(connection, member_ids)
                query_agent_ids = member_ids
                start_time_by_agent = None
            elif attribution is ObservationAttributionMode.LINEAGE_CUMULATIVE:
                query_agent_ids = _lineage_actor_ids_locked(connection, member_ids)
                start_time_by_agent = None
            elif attribution is ObservationAttributionMode.PER_SESSION:
                query_agent_ids = member_ids
                start_time_by_agent = _latest_renewal_start_times_locked(
                    connection, member_ids
                )
            else:
                raise SpiceError(
                    "teamAtEventTime attribution requires "
                    "team_historical_metric_summary"
                )
            summary = self._agent_lane_metric_summary_locked(
                connection,
                query_agent_ids,
                bucket_count=bucket_count,
                bucket_seconds=bucket_seconds,
                now=summary_time,
                start_time_by_agent=start_time_by_agent,
            )
            return LaneMetricSummary(
                agent_ids=member_ids,
                acked=summary.acked,
                sends=summary.sends,
                tool_calls=summary.tool_calls,
                sparkline=summary.sparkline,
            )

    def team_historical_metric_summary(
        self: _TeamMetricStore,
        team_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
        now: float | None = None,
        attribution: ObservationAttributionMode = (
            ObservationAttributionMode.TEAM_AT_EVENT_TIME
        ),
    ) -> TeamHistoricalMetricSummary:
        if attribution is not ObservationAttributionMode.TEAM_AT_EVENT_TIME:
            raise SpiceError(
                "team historical metrics require teamAtEventTime attribution"
            )
        team_id = _normalized_id(team_id, "team_id")
        bucket_count = max(1, int(bucket_count))
        bucket_seconds = max(1, int(bucket_seconds))
        summary_time = time.time() if now is None else max(0.0, float(now))
        with self.connect() as connection:
            intervals = [
                interval
                for interval in membership_intervals_from_events(
                    _team_event_rows_locked(connection), end_time=summary_time
                )
                if interval.team_id == team_id
            ]
            agent_ids = historical_agent_ids(intervals)
            _require_source_attribution_locked(
                connection,
                _lineage_related_actor_ids_locked(connection, agent_ids),
            )
            buckets = historical_metric_buckets(
                _agent_message_bucket_rows_locked(connection, agent_ids), intervals
            )
        return TeamHistoricalMetricSummary(
            team_id=team_id,
            agent_ids=agent_ids,
            messages=sum(buckets.values()),
            sparkline=metric_sparkline(
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
        attribution: ObservationAttributionMode = (
            ObservationAttributionMode.SOURCE_ACTOR
        ),
    ) -> tuple[MetricSeriesPoint, ...]:
        """Stable, full-fidelity activity series for graphing: summed messages
        per bucket over the given agents within [start, end]. Unlike the lane
        sparkline this applies no rolling window or aging — re-querying the same
        range always yields identical points, so it can be plotted over an
        arbitrary range (bounded only by the retention horizon)."""
        requested_ids = tuple(
            dict.fromkeys(str(agent_id) for agent_id in agent_ids if agent_id)
        )
        if not requested_ids:
            return ()
        bucket_seconds = max(1, int(bucket_seconds))
        floor = metric_bucket_start(start, bucket_seconds)
        ceiling = metric_bucket_start(end, bucket_seconds)
        with self.connect() as connection:
            if attribution is ObservationAttributionMode.SOURCE_ACTOR:
                _require_source_attribution_locked(connection, requested_ids)
                ids = requested_ids
                start_times: Mapping[str, float] = {}
            elif attribution is ObservationAttributionMode.LINEAGE_CUMULATIVE:
                ids = _lineage_actor_ids_locked(connection, requested_ids)
                start_times = {}
            elif attribution is ObservationAttributionMode.PER_SESSION:
                ids = requested_ids
                start_times = _latest_renewal_start_times_locked(connection, ids)
            else:
                raise SpiceError(
                    "teamAtEventTime attribution requires "
                    "team_historical_metric_summary"
                )
            placeholders = ",".join("?" for _ in ids)
            rows = connection.execute(
                "SELECT agent_id, bucket_start, messages "
                "FROM agent_metric_buckets "
                f"WHERE agent_id IN ({placeholders}) "
                "AND bucket_start >= ? AND bucket_start <= ? "
                "ORDER BY bucket_start",
                (*ids, floor, ceiling),
            ).fetchall()
        messages_by_bucket: Counter[int] = Counter()
        for row in rows:
            actor_id = str(row["agent_id"])
            bucket_start = int(row["bucket_start"])
            if bucket_start < start_times.get(actor_id, 0.0):
                continue
            messages_by_bucket[bucket_start] += int(row["messages"] or 0)
        return tuple(
            MetricSeriesPoint(bucket_start, messages)
            for bucket_start, messages in sorted(messages_by_bucket.items())
        )

    def task_lifecycle_series(
        self: _TeamMetricStore,
        agent_ids: Iterable[str] = (),
        *,
        team_ids: Iterable[str] = (),
        start: float,
        end: float,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
        attribution: ObservationAttributionMode = (
            ObservationAttributionMode.SOURCE_ACTOR
        ),
    ) -> tuple[TaskLifecycleSeriesPoint, ...]:
        """Stable task-flow series for graphing: task lifecycle facts folded
        into per-bucket movement counts. The substrate is append-only
        task_events tagged with actor and team-at-capture, so re-querying the
        same range yields the same projection until retention prunes it."""
        requested_agents = _normalized_ids(agent_ids, "agent_id")
        teams = _normalized_ids(team_ids, "team_id")
        if not requested_agents and not teams:
            return ()
        bucket_seconds = max(1, int(bucket_seconds))
        start_time = max(0.0, float(start))
        end_time = max(start_time, float(end))
        bucket_expr = (
            f"(CAST(ts AS INTEGER) - (CAST(ts AS INTEGER) % {bucket_seconds}))"
        )
        with self.connect() as connection:
            if attribution is ObservationAttributionMode.SOURCE_ACTOR:
                _require_source_attribution_locked(connection, requested_agents)
                agents = requested_agents
            elif attribution is ObservationAttributionMode.LINEAGE_CUMULATIVE:
                agents = _lineage_actor_ids_locked(connection, requested_agents)
            else:
                raise SpiceError(
                    "task lifecycle series supports sourceActor or "
                    "lineageCumulative attribution"
                )
            filters = ["ts >= ?", "ts <= ?"]
            params: list[object] = [start_time, end_time]
            if agents:
                filters.append(f"agent_id IN ({_placeholders(agents)})")
                params.extend(agents)
            if teams:
                filters.append(f"team_id IN ({_placeholders(teams)})")
                params.extend(teams)
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
        """Per-agent share of claimed/active in-flight tasks by bucket."""
        agents = _normalized_ids(agent_ids, "agent_id")
        teams = _normalized_ids(team_ids, "team_id")
        if not agents and not teams:
            return ()
        bucket_seconds = max(1, int(bucket_seconds))
        start_time = max(0.0, float(start))
        end_time = max(start_time, float(end))
        start_bucket = metric_bucket_start(start_time, bucket_seconds)
        end_bucket = metric_bucket_start(end_time, bucket_seconds)
        filters = ["ts <= ?"]
        params: list[object] = [end_time]
        if agents:
            filters.append(f"agent_id IN ({_placeholders(agents)})")
            params.extend(agents)
        if teams:
            filters.append(f"team_id IN ({_placeholders(teams)})")
            params.extend(teams)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT ts, kind, task_id, agent_id "
                "FROM task_events "
                f"WHERE {' AND '.join(filters)} "
                "ORDER BY ts, rowid",
                params,
            ).fetchall()
        task_states: dict[str, tuple[str, str]] = {}
        events_by_bucket: dict[int, list[sqlite3.Row]] = {}
        for row in rows:
            event_bucket = metric_bucket_start(float(row["ts"] or 0.0), bucket_seconds)
            if event_bucket < start_bucket:
                _apply_task_distribution_event(task_states, row)
            else:
                events_by_bucket.setdefault(event_bucket, []).append(row)
        points: list[TaskDistributionSeriesPoint] = []
        bucket_start = start_bucket
        while bucket_start <= end_bucket:
            for row in events_by_bucket.get(bucket_start, ()):
                _apply_task_distribution_event(task_states, row)
            counts_by_agent: dict[str, list[int]] = {}
            for agent_id, state in task_states.values():
                counts = counts_by_agent.setdefault(agent_id, [0, 0])
                if state == "claimed":
                    counts[0] += 1
                else:
                    counts[1] += 1
            total_work = sum(
                claimed + active for claimed, active in counts_by_agent.values()
            )
            for agent_id in sorted(counts_by_agent):
                claimed, active = counts_by_agent[agent_id]
                work = claimed + active
                if work <= 0:
                    continue
                points.append(
                    TaskDistributionSeriesPoint(
                        bucket_start=bucket_start,
                        agent_id=agent_id,
                        claimed=claimed,
                        active=active,
                        share=work / total_work,
                    )
                )
            bucket_start += bucket_seconds
        return tuple(points)

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

    def task_phase_effort_windows(self: _TeamMetricStore, task_rows: Iterable[dict]):
        from spice.tasks.effort import phase_effort_windows_for_tasks

        return phase_effort_windows_for_tasks(task_rows, store=self)

    def task_phase_effort_usage(
        self: _TeamMetricStore,
        task_rows: Iterable[dict],
        transcript_files_by_thread: Mapping[str, Iterable[str | Path]],
    ):
        from spice.tasks.effort import phase_effort_usage_for_tasks

        return phase_effort_usage_for_tasks(
            task_rows, transcript_files_by_thread, store=self
        )

    def task_phase_model_cost_rows(
        self: _TeamMetricStore,
        task_rows: Iterable[dict],
        transcript_files_by_thread: Mapping[str, Iterable[str | Path]],
    ):
        from spice.tasks.effort import (
            phase_effort_usage_for_tasks,
            phase_model_cost_rows,
        )

        return phase_model_cost_rows(
            phase_effort_usage_for_tasks(
                task_rows, transcript_files_by_thread, store=self
            )
        )

    def task_phase_model_cost_groups(
        self: _TeamMetricStore,
        task_rows: Iterable[dict],
        transcript_files_by_thread: Mapping[str, Iterable[str | Path]],
    ):
        from spice.tasks.effort import (
            phase_effort_usage_for_tasks,
            phase_model_cost_groups,
            phase_model_cost_rows,
        )

        return phase_model_cost_groups(
            phase_model_cost_rows(
                phase_effort_usage_for_tasks(
                    task_rows, transcript_files_by_thread, store=self
                )
            )
        )

    def _prune_metric_history_locked(
        self: _TeamMetricStore, connection: sqlite3.Connection, *, now: float
    ) -> None:
        # Bound the high-growth activity and task series at the retention
        # horizon. Directive lifecycle retention belongs to its canonical
        # steering/ACK store and is never mutated by a team snapshot prune.
        retention_seconds = _metric_history_retention_seconds_locked(connection)
        floor = int(now) - retention_seconds
        connection.execute(
            "DELETE FROM agent_metric_buckets WHERE bucket_start < ?", (floor,)
        )
        connection.execute("DELETE FROM task_events WHERE ts < ?", (float(floor),))

    def _record_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        agent_id: str,
        *,
        team_id: str,
        source_path: str,
        tool_calls: int,
        message_buckets: Counter[int],
        tool_call_buckets: Counter[int],
        now: float,
    ) -> None:
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, source_path, tool_calls, updated_at) "
            "VALUES (?, ?, ?, ?, ?) "
            "ON CONFLICT(agent_id, team_id, source_path) DO UPDATE SET "
            "tool_calls = agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = excluded.updated_at",
            (agent_id, team_id, source_path, tool_calls, now),
        )
        bucket_starts = sorted(set(message_buckets) | set(tool_call_buckets))
        for bucket_start in bucket_starts:
            connection.execute(
                "INSERT INTO agent_metric_buckets "
                "(agent_id, team_id, source_path, bucket_start, messages, tool_calls) "
                "VALUES (?, ?, ?, ?, ?, ?) "
                "ON CONFLICT(agent_id, team_id, source_path, bucket_start) "
                "DO UPDATE SET "
                "messages = agent_metric_buckets.messages + excluded.messages, "
                "tool_calls = agent_metric_buckets.tool_calls + excluded.tool_calls",
                (
                    agent_id,
                    team_id,
                    source_path,
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
            return empty_lane_metric_summary(bucket_count)
        start_times = _metric_start_times(agent_ids, start_time_by_agent)
        # sends/acked are the membership-derived directive totals (acked <= sends
        # by construction); tool_calls is the per-agent activity counter.
        directives = self._directive_totals_for_agents_locked(
            connection, agent_ids, start_time_by_agent=start_times
        )
        lifetime_tool_calls = _lifetime_tool_calls_locked(connection, agent_ids)
        # Only buckets inside the sparkline window contribute, so bound the read
        # there instead of scanning the agent's whole (unbounded) bucket history
        # on every render. Mirror metric_sparkline's window start exactly.
        window_floor = metric_bucket_start(now, bucket_seconds) - (
            (bucket_count - 1) * bucket_seconds
        )
        message_buckets, window_tool_calls = _lane_activity_buckets_locked(
            connection,
            agent_ids,
            window_floor=window_floor,
            start_time_by_agent=start_times,
        )
        return lane_metric_summary_from_buckets(
            agent_ids,
            message_buckets.items(),
            acked=directives.acked,
            sends=directives.sends,
            tool_calls=window_tool_calls if start_times else lifetime_tool_calls,
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
            now=now,
        )


def _record_agent_metric_cursor_locked(
    connection: sqlite3.Connection,
    agent_id: str,
    *,
    checkpoint: AgentMetricCheckpoint,
    now: float,
) -> None:
    identity = checkpoint.file_identity
    connection.execute(
        "INSERT INTO agent_metric_cursors "
        "(agent_id, source_path, offset, source_device, source_inode, "
        "updated_at) VALUES (?, ?, ?, ?, ?, ?) "
        "ON CONFLICT(agent_id, source_path) DO UPDATE SET "
        "offset = excluded.offset, "
        "source_device = excluded.source_device, "
        "source_inode = excluded.source_inode, "
        "updated_at = excluded.updated_at",
        (
            agent_id,
            checkpoint.source_path,
            max(0, int(checkpoint.offset)),
            None if identity is None else identity.device,
            None if identity is None else identity.inode,
            now,
        ),
    )


def _reset_unaccountable_metrics_locked(
    connection: sqlite3.Connection, agent_id: str, *, source_path: str
) -> None:
    """Clear what one source contributed once its checkpoint is gone.

    Facts and the checkpoint covering them are written together, so counts
    standing beside a missing checkpoint mean the checkpoint was lost -- a
    dropped table after a shape change, a deleted row, a restored database
    missing one file. The caller is resuming that source from its first byte
    because of it, so the counts that source already produced are exactly what
    the replay is about to produce again: they are cleared here, inside the
    transaction that replaces them.

    Only that source's counts go. Every other source the agent reads is still
    covered by its own checkpoint, so nothing is going to replay it, and
    clearing it would erase activity that never comes back.
    """
    checkpointed = connection.execute(
        "SELECT 1 FROM agent_metric_cursors WHERE agent_id = ? AND source_path = ?",
        (agent_id, source_path),
    ).fetchone()
    if checkpointed is not None:
        return
    connection.execute(
        "DELETE FROM agent_metrics WHERE agent_id = ? AND source_path = ?",
        (agent_id, source_path),
    )
    connection.execute(
        "DELETE FROM agent_metric_buckets WHERE agent_id = ? AND source_path = ?",
        (agent_id, source_path),
    )


def _file_identity(
    device: int | None, inode: int | None
) -> TranscriptFileIdentity | None:
    """Rebuild a stored source identity, absent until both halves are present."""
    if device is None or inode is None:
        return None
    return TranscriptFileIdentity(device=int(device), inode=int(inode))


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
    env_value = os.environ.get(
        METRIC_HISTORY_RETENTION_DAYS_ENV, ""
    ).strip()  # env-policy: allow
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
    if not math.isfinite(days):
        raise SpiceError(f"{field_name} must be finite")
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


def _apply_task_distribution_event(
    task_states: dict[str, tuple[str, str]], row: sqlite3.Row
) -> None:
    task_id = str(row["task_id"])
    kind = str(row["kind"])
    if kind == "claim":
        task_states[task_id] = (str(row["agent_id"]), "claimed")
    elif kind in {"phaseAdvance", "review"}:
        task_states[task_id] = (str(row["agent_id"]), "active")
    elif kind in {"complete", "drain"}:
        task_states.pop(task_id, None)


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
    query_floor = min(metric_bucket_start(claim.claimed_at) for claim in claims)
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
    activity_floor = metric_bucket_start(claim.claimed_at)
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


def _team_event_rows_locked(connection: sqlite3.Connection) -> list[sqlite3.Row]:
    return connection.execute(
        "SELECT ts, kind, team_id, payload FROM events ORDER BY revision"
    ).fetchall()


def _agent_message_bucket_rows_locked(
    connection: sqlite3.Connection, agent_ids: tuple[str, ...]
) -> list[sqlite3.Row]:
    if not agent_ids:
        return []
    return connection.execute(
        "SELECT agent_id, bucket_start, messages FROM agent_metric_buckets "
        f"WHERE agent_id IN ({_placeholders(agent_ids)}) ORDER BY bucket_start",
        agent_ids,
    ).fetchall()


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
        payload = event_payload(row)
        successor = event_agent_id(payload, "successor")
        if successor not in wanted:
            continue
        start_times[successor] = max(
            start_times.get(successor, 0.0),
            float(row["ts"] or 0.0),
        )
    return start_times


def _lineage_edges_locked(
    connection: sqlite3.Connection,
) -> tuple[dict[str, list[str]], dict[str, list[str]], dict[str, float]]:
    rows = connection.execute(
        "SELECT ts, kind, payload FROM events "
        "WHERE kind IN ('renewalStarted', 'assignAgent') ORDER BY revision"
    ).fetchall()
    predecessors: dict[str, list[str]] = {}
    successors: dict[str, list[str]] = {}
    successor_starts: dict[str, float] = {}
    for row in rows:
        payload = event_payload(row)
        kind = str(row["kind"])
        if kind == "renewalStarted":
            successor = event_agent_id(payload, "successor")
            sources = (event_agent_id(payload, "predecessor"),)
            starts_session = True
        else:
            successor = event_agent_id(payload, "agentId")
            raw_aliases = payload.get("aliases", [])
            if not isinstance(raw_aliases, list) or not all(
                isinstance(alias, str) and alias for alias in raw_aliases
            ):
                raise SpiceError(
                    "team event payload aliases must be a list of agent ids"
                )
            sources = tuple(str(alias) for alias in raw_aliases)
            starts_session = False
        for source in sources:
            if source == successor:
                continue
            parents = predecessors.setdefault(successor, [])
            if source not in parents:
                parents.append(source)
            children = successors.setdefault(source, [])
            if successor not in children:
                children.append(successor)
            if starts_session:
                successor_starts[successor] = min(
                    successor_starts.get(successor, float(row["ts"] or 0.0)),
                    float(row["ts"] or 0.0),
                )
    return predecessors, successors, successor_starts


def _lineage_actor_ids_locked(
    connection: sqlite3.Connection,
    actor_ids: tuple[str, ...],
) -> tuple[str, ...]:
    predecessors, _, _ = _lineage_edges_locked(connection)
    ordered: list[str] = []
    visited: set[str] = set()
    visiting: set[str] = set()

    def visit(actor_id: str) -> None:
        if actor_id in visited:
            return
        if actor_id in visiting:
            raise SpiceError(f"observation lineage contains a cycle at {actor_id}")
        visiting.add(actor_id)
        for predecessor in predecessors.get(actor_id, ()):
            visit(predecessor)
        visiting.remove(actor_id)
        visited.add(actor_id)
        ordered.append(actor_id)

    for actor_id in actor_ids:
        visit(actor_id)
    return tuple(ordered)


def _lineage_related_actor_ids_locked(
    connection: sqlite3.Connection,
    actor_ids: tuple[str, ...],
) -> tuple[str, ...]:
    predecessors, successors, _ = _lineage_edges_locked(connection)
    related: list[str] = []
    seen: set[str] = set()
    pending = list(actor_ids)
    while pending:
        actor_id = pending.pop(0)
        if actor_id in seen:
            continue
        seen.add(actor_id)
        related.append(actor_id)
        pending.extend(predecessors.get(actor_id, ()))
        pending.extend(successors.get(actor_id, ()))
    return tuple(related)


def _require_source_attribution_locked(
    connection: sqlite3.Connection,
    actor_ids: tuple[str, ...],
) -> None:
    state = connection.execute(
        "SELECT status FROM observation_attribution_state WHERE singleton = 1"
    ).fetchone()
    if (
        state is not None
        and str(state["status"]) == OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED
    ):
        raise SpiceError(OBSERVATION_SOURCE_REBUILD_REQUIRED)
    _, _, successor_starts = _lineage_edges_locked(connection)
    for actor_id in actor_ids:
        start = successor_starts.get(actor_id)
        if start is None:
            continue
        row = connection.execute(
            "SELECT 1 FROM agent_metric_buckets "
            "WHERE agent_id = ? AND bucket_start < ? "
            "UNION ALL SELECT 1 FROM agent_metrics "
            "WHERE agent_id = ? AND updated_at < ? "
            "UNION ALL SELECT 1 FROM task_events "
            "WHERE agent_id = ? AND ts < ? LIMIT 1",
            (actor_id, start, actor_id, start, actor_id, start),
        ).fetchone()
        if row is not None:
            raise SpiceError(OBSERVATION_SOURCE_REBUILD_REQUIRED)


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
