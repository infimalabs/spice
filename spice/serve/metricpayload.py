"""Graphable metric series payloads for the live bus."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

from spice.errors import SpiceError
from spice.serve.teammetrics import METRIC_BUCKET_SECONDS

SERIES_METRICS = frozenset(
    {"activity", "sends", "acks", "burndown", "distribution", "stuck", "drained"}
)
SERIES_LENSES = frozenset({"lineage", "perSession", "teamHistorical"})
TASK_METRIC_FIELDS = {
    "burndown": "completed",
    "distribution": "claimed",
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
        state.team_store,
        metric=metric,
        lens=lens,
        subject=subject,
        start=series_start,
        end=end,
        bucket_seconds=bucket_seconds,
    )
    return {
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


def _series_points(
    store: Any,
    *,
    metric: str,
    lens: str,
    subject: _SeriesSubject,
    start: float,
    end: float,
    bucket_seconds: int,
) -> list[dict[str, Any]]:
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
        return float(value)
    except (TypeError, ValueError) as exc:
        raise SpiceError(f"{field_name} must be numeric") from exc


def _bucket_start(timestamp: float, bucket_seconds: int) -> int:
    raw = max(0, int(float(timestamp)))
    return raw - (raw % max(1, int(bucket_seconds)))
