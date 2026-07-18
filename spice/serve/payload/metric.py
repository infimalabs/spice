"""Graphable metric series payloads for the live bus."""

from __future__ import annotations

import json
import math
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Any, cast

from spice.errors import SpiceError
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.team.history import (
    METRIC_BUCKET_SECONDS,
    TEAM_HISTORICAL_MAX_BUCKET_COUNT,
)

SERIES_METRICS = frozenset(
    {
        "activity",
        "sends",
        "acks",
        "burndown",
        "distribution",
        "stuck",
        "drained",
        "phaseEffort",
    }
)
SERIES_LENSES = frozenset({"lineage", "perSession", "teamHistorical"})
TASK_METRIC_FIELDS = {
    "burndown": "completed",
    "stuck": "active",
    "drained": "drained",
}


@dataclass(frozen=True)
class _SeriesSubject:
    scope: str
    agent_ids: tuple[str, ...]
    team_ids: tuple[str, ...]
    payload: dict[str, Any]


def metric_series_payload(state: Any, query: dict[str, Any]) -> dict[str, Any]:
    metric = _series_choice(query.get("metric"), SERIES_METRICS, "metric")
    lens = _series_choice(query.get("lens") or "lineage", SERIES_LENSES, "lens")
    start = max(0.0, _float_value(query.get("start"), "start"))
    end = max(start, _float_value(query.get("end"), "end"))
    bucket_seconds = max(1, int(query.get("bucketSeconds") or METRIC_BUCKET_SECONDS))
    subject = _series_subject(state.team_store, query)
    series_start = _effective_series_start(state.team_store, subject, lens, start)
    points = _series_points(
        state,
        metric=metric,
        lens=lens,
        subject=subject,
        start=series_start,
        end=end,
        bucket_seconds=bucket_seconds,
    )
    payload = {
        "ok": True,
        "metric": metric,
        "lens": lens,
        "start": start,
        "effectiveStart": series_start,
        "end": end,
        "bucketSeconds": bucket_seconds,
        "subject": subject.payload,
        "points": points,
    }
    return validate_emitter_payload("payload.metric.metric_series_payload", payload)


def _series_points(
    state: Any,
    *,
    metric: str,
    lens: str,
    subject: _SeriesSubject,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    store = state.team_store
    if metric == "activity":
        return _activity_points(
            store,
            lens=lens,
            subject=subject,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    if metric in {"sends", "acks"}:
        return _directive_points(
            store,
            subject,
            metric=metric,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    if metric == "distribution":
        return _distribution_points(
            store,
            subject=subject,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    if metric == "phaseEffort":
        return _phase_effort_points(
            state,
            store,
            subject=subject,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    return _task_points(
        store,
        subject=subject,
        metric=metric,
        start=start,
        end=end,
        bucket_seconds=bucket_seconds,
    )


def _activity_points(
    store: Any,
    *,
    lens: str,
    subject: _SeriesSubject,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    if lens == "teamHistorical":
        if len(subject.team_ids) != 1:
            raise SpiceError("teamHistorical activity series requires one teamId")
        return _historical_activity_points(
            store,
            subject.team_ids[0],
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    return [
        {
            "bucketStart": point.bucket_start,
            "value": point.messages,
            "messages": point.messages,
        }
        for point in store.agent_activity_series(
            subject.agent_ids,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    ]


def _historical_activity_points(
    store: Any,
    team_id: str,
    *,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    start_bucket = _bucket_start(start, bucket_seconds)
    end_bucket = _bucket_start(end, bucket_seconds)
    bucket_count = ((end_bucket - start_bucket) // bucket_seconds) + 1
    if bucket_count > TEAM_HISTORICAL_MAX_BUCKET_COUNT:
        raise SpiceError(
            "teamHistorical activity series range exceeds "
            f"{TEAM_HISTORICAL_MAX_BUCKET_COUNT} buckets"
        )
    summary = store.team_historical_metric_summary(
        team_id,
        bucket_count=bucket_count,
        bucket_seconds=bucket_seconds,
        now=end,
    )
    first_bucket = end_bucket - ((len(summary.sparkline) - 1) * bucket_seconds)
    return [
        {"bucketStart": bucket, "value": messages, "messages": messages}
        for index, messages in enumerate(summary.sparkline)
        if (bucket := first_bucket + (index * bucket_seconds)) >= start_bucket
        and messages
    ]


def _directive_points(
    store: Any,
    subject: _SeriesSubject,
    *,
    metric: str,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    field = "team_id" if subject.scope == "team" else "agent_id"
    ids = subject.team_ids if subject.scope == "team" else subject.agent_ids
    if not ids:
        return []
    placeholders = ",".join("?" for _id in ids)
    timestamp_column = "sent_at" if metric == "sends" else "acked_at"
    ack_filter = "AND acked = 1 " if metric == "acks" else ""
    bucket_expr = (
        f"CAST({timestamp_column} AS INTEGER) - "
        f"(CAST({timestamp_column} AS INTEGER) % ?)"
    )
    with store.connect() as connection:
        rows = connection.execute(
            f"SELECT {bucket_expr} AS bucket_start, COUNT(*) AS count FROM directives "
            f"WHERE {field} IN ({placeholders}) "
            f"AND {timestamp_column} >= ? AND {timestamp_column} <= ? "
            f"{ack_filter}"
            "GROUP BY bucket_start ORDER BY bucket_start",
            (bucket_seconds, *ids, start, end),
        ).fetchall()
    return [
        {
            "bucketStart": int(row["bucket_start"]),
            "value": int(row["count"] or 0),
            metric: int(row["count"] or 0),
        }
        for row in rows
    ]


def _task_points(
    store: Any,
    *,
    subject: _SeriesSubject,
    metric: str,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    field = TASK_METRIC_FIELDS[metric]
    agent_ids = () if subject.scope == "team" else subject.agent_ids
    team_ids = subject.team_ids if subject.scope == "team" else ()
    return [
        {
            "bucketStart": point.bucket_start,
            "value": int(getattr(point, field)),
            "claimed": point.claimed,
            "active": point.active,
            "completed": point.completed,
            "drained": point.drained,
        }
        for point in store.task_lifecycle_series(
            agent_ids,
            team_ids=team_ids,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
        if int(getattr(point, field))
    ]


def _distribution_points(
    store: Any,
    *,
    subject: _SeriesSubject,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    agent_ids = () if subject.team_ids else subject.agent_ids
    return [
        {
            "bucketStart": point.bucket_start,
            "agentId": point.agent_id,
            "value": point.share,
            "share": point.share,
            "claimed": point.claimed,
            "active": point.active,
            "work": point.claimed + point.active,
        }
        for point in store.task_distribution_series(
            agent_ids,
            team_ids=subject.team_ids,
            start=start,
            end=end,
            bucket_seconds=bucket_seconds,
        )
    ]


def _phase_effort_points(
    state: Any,
    store: Any,
    *,
    subject: _SeriesSubject,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
    task_rows = _phase_effort_task_rows(state)
    windows = store.task_phase_effort_windows(task_rows)
    files_by_thread = _phase_effort_transcript_files_by_thread(state, windows)
    usage_rows = store.task_phase_effort_usage(task_rows, files_by_thread)
    return [
        _phase_effort_point(usage, bucket_seconds=bucket_seconds)
        for usage in usage_rows
        if _phase_effort_usage_matches_subject(usage, subject)
        if _phase_effort_usage_overlaps_range(usage, start=start, end=end)
    ]


def _phase_effort_task_rows(state: Any) -> list[dict[str, Any]]:
    configured = getattr(state, "phase_effort_task_rows", None)
    if callable(configured):
        configured = configured()
    if isinstance(configured, Iterable):
        return [dict(cast(Mapping[str, Any], row)) for row in configured]
    from spice.tasks import tw

    return tw.export()


def _phase_effort_transcript_files_by_thread(
    state: Any, windows: tuple[Any, ...]
) -> dict[str, tuple[Any, ...]]:
    configured = getattr(state, "phase_effort_transcript_files_by_thread", None)
    if callable(configured):
        return _normalized_transcript_files_by_thread(configured(windows))
    if configured is not None:
        return _normalized_transcript_files_by_thread(configured)

    from spice.serve.messages import resolve_thread_transcript

    files_by_thread: dict[str, tuple[Any, ...]] = {}
    repo_roots = _phase_effort_repo_roots(state)
    for thread_id in sorted(
        {window.thread_id for window in windows if window.thread_id}
    ):
        for repo_root in repo_roots:
            resolution = resolve_thread_transcript(thread_id, repo_root)
            if resolution is not None:
                files_by_thread[thread_id] = (resolution.path,)
                break
    return files_by_thread


def _normalized_transcript_files_by_thread(raw: Any) -> dict[str, tuple[Any, ...]]:
    return {
        str(thread_id): _transcript_path_tuple(paths)
        for thread_id, paths in dict(raw or {}).items()
        if str(thread_id)
    }


def _transcript_path_tuple(paths: Any) -> tuple[Any, ...]:
    if isinstance(paths, str):
        return (paths,)
    try:
        return tuple(paths)
    except TypeError:
        return (paths,)


def _phase_effort_repo_roots(state: Any) -> tuple[Any, ...]:
    worktree_targets = getattr(state, "worktree_targets", None)
    if not callable(worktree_targets):
        return (None,)
    targets = worktree_targets()
    if not isinstance(targets, Iterable):
        return (None,)
    roots = tuple(
        target.repo_root
        for target in targets
        if getattr(target, "repo_root", None) is not None
    )
    return roots or (None,)


def _phase_effort_usage_matches_subject(usage: Any, subject: _SeriesSubject) -> bool:
    if subject.scope == "team":
        return usage.window.team_id in subject.team_ids
    return usage.actor_id in subject.agent_ids


def _phase_effort_usage_overlaps_range(usage: Any, *, start: float, end: float) -> bool:
    started_at = usage.window.started_at
    ended_at = usage.window.ended_at
    first = started_at if started_at is not None else ended_at
    last = ended_at if ended_at is not None else started_at
    if last is not None and last < start:
        return False
    if first is not None and first > end:
        return False
    return True


def _phase_effort_point(usage: Any, *, bucket_seconds: int) -> dict[str, Any]:
    window = usage.window
    return {
        "bucketStart": _bucket_start(_phase_effort_bucket_time(usage), bucket_seconds),
        "value": usage.total_tokens,
        "taskId": usage.task_id,
        "handle": usage.handle,
        "title": window.title,
        "phase": usage.phase,
        "phaseIndex": usage.phase_index,
        "agentId": usage.actor_id,
        "threadId": usage.thread_id,
        "teamId": window.team_id,
        "driver": usage.driver,
        "model": usage.model,
        "effort": usage.effort,
        "startedAt": window.started_at,
        "endedAt": window.ended_at,
        "wallSeconds": usage.wall_seconds,
        "inputTokens": usage.input_tokens,
        "cachedInputTokens": usage.cached_input_tokens,
        "outputTokens": usage.output_tokens,
        "reasoningOutputTokens": usage.reasoning_output_tokens,
        "totalTokens": usage.total_tokens,
        "turns": usage.turn_count,
        "messages": usage.message_count,
        "renewals": usage.renewal_count,
        "sourceFiles": list(usage.source_files),
        "partial": usage.partial,
        "partialMarkers": list(usage.partial_markers),
    }


def _phase_effort_bucket_time(usage: Any) -> float:
    window = usage.window
    if window.started_at is not None:
        return float(window.started_at)
    if window.ended_at is not None:
        return float(window.ended_at)
    return 0.0


def _series_subject(store: Any, query: dict[str, Any]) -> _SeriesSubject:
    team_id = str(query.get("teamId") or "").strip()
    agent_id = str(query.get("agentId") or query.get("lane") or "").strip()
    if team_id:
        state = store.team_state(team_id)
        agent_ids = tuple(member.agent_id for member in state.members)
        return _SeriesSubject(
            scope="team",
            agent_ids=agent_ids,
            team_ids=(team_id,),
            payload={"teamId": team_id, "agentIds": list(agent_ids)},
        )
    if not agent_id:
        raise SpiceError("metric series requires agentId, teamId, or lane")
    agent_ids, current_team_id = _lane_agent_ids(store, agent_id)
    payload: dict[str, Any] = {"agentId": agent_id, "agentIds": list(agent_ids)}
    team_ids: tuple[str, ...] = ()
    if current_team_id:
        payload["teamId"] = current_team_id
        team_ids = (current_team_id,)
    return _SeriesSubject(
        scope="agent", agent_ids=agent_ids, team_ids=team_ids, payload=payload
    )


def _lane_agent_ids(store: Any, agent_id: str) -> tuple[tuple[str, ...], str | None]:
    with store.connect() as connection:
        row = connection.execute(
            "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
        ).fetchone()
        if row is None:
            return (agent_id,), None
        team_id = str(row["team_id"])
        member_rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY position",
            (team_id,),
        ).fetchall()
    return tuple(str(member["agent_id"]) for member in member_rows), team_id


def _effective_series_start(
    store: Any, subject: _SeriesSubject, lens: str, start: float
) -> float:
    if lens != "perSession" or subject.scope != "agent" or not subject.agent_ids:
        return start
    with store.connect() as connection:
        rows = connection.execute(
            "SELECT ts, payload FROM events "
            "WHERE kind = 'renewalStarted' ORDER BY revision"
        ).fetchall()
    successors = set(subject.agent_ids)
    starts = []
    for row in rows:
        payload = json.loads(str(row["payload"] or "{}"))
        if not isinstance(payload, dict):
            raise SpiceError("team event payload must be a JSON object")
        if payload.get("successor") in successors:
            starts.append(float(row["ts"] or 0.0))
    return max(start, max(starts)) if starts else start


def _series_choice(value: Any, allowed: frozenset[str], field_name: str) -> str:
    choice = str(value or "").strip()
    if choice not in allowed:
        raise SpiceError(f"{field_name} must be one of {', '.join(sorted(allowed))}")
    return choice


def _float_value(value: Any, field_name: str) -> float:
    try:
        parsed = float(value)
    except (TypeError, ValueError) as exc:
        raise SpiceError(f"{field_name} must be numeric") from exc
    if not math.isfinite(parsed):
        raise SpiceError(f"{field_name} must be finite")
    return parsed


def _bucket_start(timestamp: float, bucket_seconds: int) -> int:
    raw = max(0, int(float(timestamp)))
    return raw - (raw % max(1, int(bucket_seconds)))
