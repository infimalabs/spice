"""Serve HTTP payloads, route parsing, metrics, and lane watch signatures."""

from __future__ import annotations

import errno
import math
import time
from http import HTTPStatus
from io import BufferedReader
from pathlib import Path
from socket import SocketIO
from typing import Any
from urllib.parse import unquote, urlparse

from spice.agent.lifecycle import agent_state_path
from spice.errors import SpiceError
from spice.mail.attachments import resolve_shared_attachment_ref
from spice.mail.inbox import collect_inbox_items, inbox_dir, pending_inbox_count
from spice.mail.replies import ensure_reply_log
from spice.paths import shared_attachment_root
from spice.serve.livebus import LaneSignature
from spice.serve.messages import TranscriptResolution, resolve_thread_transcript
from spice.serve.payload import identity
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.team.history import (
    METRIC_BUCKET_SECONDS,
    TEAM_HISTORICAL_MAX_BUCKET_COUNT,
    metric_bucket_start,
)
from spice.serve.workroutes import resolve_worktree_for_request
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import config as task_config

STATIC_ASSET_ROUTE_PREFIX = "/static/"
METRICS_CONTENT_TYPE = "text/plain; version=0.0.4; charset=utf-8"
TEAM_HISTORICAL_METRIC_BUCKET_COUNT = 12
TASK_BURNDOWN_BUCKET_COUNT = 12
TASK_BURNDOWN_MAX_BUCKET_COUNT = 1440
TASK_DISTRIBUTION_BUCKET_COUNT = 12
TASK_DISTRIBUTION_MAX_BUCKET_COUNT = 1440
WORK_TREE_API_METRIC_ACTIONS = frozenset(
    {
        "",
        "agent/ensure",
        "agent/status",
        "files/image",
        "messages",
        "messages/image",
        "say",
        "send",
    }
)
MISSING_IMAGE_PLACEHOLDER_SVG = (
    b'<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180" '
    b'role="img" aria-label="Image unavailable">'
    b'<rect width="320" height="180" fill="#111814"/>'
    b'<rect x="12" y="12" width="296" height="156" rx="10" '
    b'fill="#18211c" stroke="#4b6255"/>'
    b'<text x="160" y="86" fill="#d7e6dc" '
    b'font-family="system-ui, sans-serif" font-size="18" '
    b'text-anchor="middle">Image unavailable</text>'
    b'<text x="160" y="112" fill="#8aa091" '
    b'font-family="system-ui, sans-serif" font-size="13" '
    b'text-anchor="middle">The referenced file is no longer present.</text>'
    b"</svg>"
)
_CLIENT_DISCONNECT_ERRNOS = frozenset(
    {errno.EBADF, errno.ECONNRESET, errno.EPIPE, errno.ECONNABORTED}
)


def team_snapshot_response_payload(
    state: Any, *, since_revision: int | None
) -> dict[str, Any]:
    snapshot = state.team_store.team_snapshot(since_revision=since_revision)
    changed = since_revision is None or snapshot.global_revision > since_revision
    payload = {
        "ok": True,
        "revision": snapshot.global_revision,
        "changed": changed,
    }
    if changed:
        if since_revision is None:
            payload["snapshot"] = snapshot.to_payload()
        else:
            payload["differential"] = True
            payload["snapshot"] = state.team_store.team_snapshot_delta_payload(
                snapshot, since_revision=since_revision
            )
    return validate_emitter_payload("httpapi.team_snapshot_response_payload", payload)


def team_command_response_payload(
    state: Any, payload: dict[str, Any]
) -> tuple[dict[str, Any], HTTPStatus]:
    try:
        normalized = identity.normalize_team_command_payload(
            payload, targets=state.worktree_targets()
        )
        result = state.team_commands.apply(normalized)
    except SpiceError as exc:
        payload = validate_emitter_payload(
            "httpapi.team_command_response_payload",
            {"ok": False, "error": str(exc)},
        )
        return payload, HTTPStatus.CONFLICT
    previous_revision = normalized.get("expectedRevision")
    differential = previous_revision is not None
    snapshot_payload = (
        state.team_store.team_snapshot_delta_payload(
            result.snapshot, since_revision=int(previous_revision)
        )
        if differential
        else result.snapshot.to_payload()
    )
    payload = validate_emitter_payload(
        "httpapi.team_command_response_payload",
        {
            "ok": True,
            "revision": result.revision,
            "snapshot": snapshot_payload,
            "differential": differential,
        },
    )
    return (
        payload,
        HTTPStatus.OK,
    )


def team_historical_metrics_response_payload(
    state: Any,
    team_id: str,
    query: dict[str, list[str]],
) -> dict[str, Any]:
    bucket_seconds = _query_int(query, "bucketSeconds", METRIC_BUCKET_SECONDS)
    summary_time = _query_strict_finite_float(query, "end", minimum=0.0)
    if summary_time is None:
        summary_time = _query_strict_finite_float(query, "now", minimum=0.0)
    if summary_time is None:
        summary_time = time.time()
    raw_start = _query_strict_finite_float(query, "start", minimum=0.0)
    if raw_start is None:
        bucket_count = _query_int(
            query,
            "bucketCount",
            TEAM_HISTORICAL_METRIC_BUCKET_COUNT,
        )
        if bucket_count > TEAM_HISTORICAL_MAX_BUCKET_COUNT:
            raise SpiceError(
                "team historical metrics bucketCount exceeds "
                f"{TEAM_HISTORICAL_MAX_BUCKET_COUNT} buckets"
            )
    else:
        bucket_count = _metric_bucket_count_for_range(
            raw_start,
            summary_time,
            bucket_seconds,
        )
        if bucket_count > TEAM_HISTORICAL_MAX_BUCKET_COUNT:
            raise SpiceError(
                "team historical metrics range exceeds "
                f"{TEAM_HISTORICAL_MAX_BUCKET_COUNT} buckets"
            )
    summary = state.team_store.team_historical_metric_summary(
        team_id,
        bucket_count=bucket_count,
        bucket_seconds=bucket_seconds,
        now=summary_time,
    )
    window_end = metric_bucket_start(summary_time, bucket_seconds)
    window_start = window_end - ((len(summary.sparkline) - 1) * bucket_seconds)
    series = [
        {"bucketStart": window_start + (index * bucket_seconds), "messages": count}
        for index, count in enumerate(summary.sparkline)
    ]
    range_messages = sum(summary.sparkline)
    return {
        "ok": True,
        "lens": "team-historical",
        "teamId": summary.team_id,
        "agentIds": list(summary.agent_ids),
        "messages": range_messages,
        "cumulativeMessages": summary.messages,
        "bucketSeconds": bucket_seconds,
        "bucketCount": len(summary.sparkline),
        "range": {"start": window_start, "end": window_end},
        "sparkline": list(summary.sparkline),
        "series": series,
    }


def task_burndown_metrics_response_payload(
    state: Any,
    query: dict[str, list[str]],
) -> dict[str, Any]:
    bucket_seconds = _query_int(query, "bucketSeconds", METRIC_BUCKET_SECONDS)
    end_time = _query_finite_float(query, "end", None, minimum=0.0)
    if end_time is None:
        end_time = _query_finite_float(query, "now", None, minimum=0.0)
    if end_time is None:
        end_time = time.time()
    raw_start = _query_finite_float(query, "start", None, minimum=0.0)
    if raw_start is None:
        bucket_count = _query_int(query, "bucketCount", TASK_BURNDOWN_BUCKET_COUNT)
        bucket_count = min(bucket_count, TASK_BURNDOWN_MAX_BUCKET_COUNT)
        window_end = metric_bucket_start(end_time, bucket_seconds)
        window_start = max(0, window_end - ((bucket_count - 1) * bucket_seconds))
    else:
        bucket_count = _metric_bucket_count_for_range(
            raw_start,
            end_time,
            bucket_seconds,
        )
        if bucket_count > TASK_BURNDOWN_MAX_BUCKET_COUNT:
            raise SpiceError(
                f"task burndown range exceeds {TASK_BURNDOWN_MAX_BUCKET_COUNT} buckets"
            )
        window_start = metric_bucket_start(raw_start, bucket_seconds)
        window_end = metric_bucket_start(end_time, bucket_seconds)
    agent_ids = _query_values(query, "agentId")
    team_ids = _query_values(query, "teamId")
    series = state.team_store.task_lifecycle_series(
        agent_ids,
        team_ids=team_ids,
        start=window_start,
        end=window_end,
        bucket_seconds=bucket_seconds,
    )
    points = [
        {
            "bucketStart": point.bucket_start,
            "completed": point.completed,
            "drained": point.drained,
        }
        for point in series
    ]
    completed = sum(point.completed for point in series)
    drained = sum(point.drained for point in series)
    return {
        "ok": True,
        "lens": "task-burndown",
        "agentIds": list(agent_ids),
        "teamIds": list(team_ids),
        "completed": completed,
        "drained": drained,
        "bucketSeconds": bucket_seconds,
        "bucketCount": bucket_count,
        "range": {"start": window_start, "end": window_end},
        "series": points,
    }


def task_distribution_metrics_response_payload(
    state: Any,
    query: dict[str, list[str]],
) -> dict[str, Any]:
    bucket_seconds = _query_int(query, "bucketSeconds", METRIC_BUCKET_SECONDS)
    end_time = _query_finite_float(query, "end", None, minimum=0.0)
    if end_time is None:
        end_time = _query_finite_float(query, "now", None, minimum=0.0)
    if end_time is None:
        end_time = time.time()
    raw_start = _query_finite_float(query, "start", None, minimum=0.0)
    if raw_start is None:
        bucket_count = _query_int(query, "bucketCount", TASK_DISTRIBUTION_BUCKET_COUNT)
        bucket_count = min(bucket_count, TASK_DISTRIBUTION_MAX_BUCKET_COUNT)
        window_end = metric_bucket_start(end_time, bucket_seconds)
        window_start = max(0, window_end - ((bucket_count - 1) * bucket_seconds))
    else:
        bucket_count = _metric_bucket_count_for_range(
            raw_start,
            end_time,
            bucket_seconds,
        )
        if bucket_count > TASK_DISTRIBUTION_MAX_BUCKET_COUNT:
            raise SpiceError(
                "task distribution range exceeds "
                f"{TASK_DISTRIBUTION_MAX_BUCKET_COUNT} buckets"
            )
        window_start = metric_bucket_start(raw_start, bucket_seconds)
        window_end = metric_bucket_start(end_time, bucket_seconds)
    agent_ids = _query_values(query, "agentId")
    team_ids = _query_values(query, "teamId")
    series = state.team_store.task_distribution_series(
        agent_ids,
        team_ids=team_ids,
        start=window_start,
        end=window_end,
        bucket_seconds=bucket_seconds,
    )
    points = [
        {
            "bucketStart": point.bucket_start,
            "agentId": point.agent_id,
            "claimed": point.claimed,
            "active": point.active,
            "work": point.claimed + point.active,
            "share": point.share,
        }
        for point in series
    ]
    claimed = sum(point.claimed for point in series)
    active = sum(point.active for point in series)
    return {
        "ok": True,
        "lens": "task-distribution",
        "agentIds": list(agent_ids),
        "teamIds": list(team_ids),
        "claimed": claimed,
        "active": active,
        "work": claimed + active,
        "bucketSeconds": bucket_seconds,
        "bucketCount": bucket_count,
        "range": {"start": window_start, "end": window_end},
        "series": points,
    }


def lane_watch_paths_for_target(
    state: Any,
    target: WorktreeTarget,
    thread_id: str | None,
    transcript: TranscriptResolution | None,
) -> tuple[Path, ...]:
    del state
    target_inbox = inbox_dir(target.repo_root)
    target_inbox.mkdir(parents=True, exist_ok=True)
    paths = [target_inbox, task_config.ensure_task_event_file()]
    agent_state = _agent_state_signature_path(target.repo_root)
    if agent_state is not None:
        paths.append(agent_state)
    if transcript is not None:
        paths.append(transcript.path)
    if thread_id:
        reply_log = ensure_reply_log(target.repo_root, thread_id)
        if reply_log is not None:
            paths.append(reply_log)
    return tuple(paths)


def lane_signature_for_target(
    state: Any,
    target: WorktreeTarget,
    thread_id: str | None,
    transcript: TranscriptResolution | None,
) -> LaneSignature:
    team_facts = identity.team_facts_for_target(state.team_store, target, thread_id)
    return LaneSignature(
        transcript=(
            _path_signature(transcript.path if transcript else None),
            transcript.owner_driver.name if transcript else "",
        ),
        inbox=_inbox_signature(target.repo_root),
        other=(
            team_facts.get("teamId", ""),
            team_facts.get("teamRevision", 0),
            team_facts.get("configRevision", 0),
            tuple(team_facts.get("taskFilters", [])),
            team_facts.get("lifetime", ""),
            tuple(
                (team_facts.get("renewalIntent") or {}).get(key, "")
                for key in (
                    "requested",
                    "state",
                    "ancestorThreadId",
                    "successorAgentId",
                    "revision",
                )
            ),
            _path_signature(task_config.ensure_task_event_file()),
            _reply_log_signature(target.repo_root, thread_id),
            _path_signature(_agent_state_signature_path(target.repo_root)),
        ),
    )


def _agent_state_signature_path(repo_root: Path) -> Path | None:
    try:
        return agent_state_path(repo_root)
    except SpiceError:
        return None


def _reply_log_signature(
    repo_root: Path, thread_id: str | None
) -> tuple[str, int, int]:
    if not thread_id:
        return ("", 0, 0)
    return _path_signature(ensure_reply_log(repo_root, thread_id))


def _path_signature(path: Path | None) -> tuple[str, int, int]:
    if path is None:
        return ("", 0, 0)
    try:
        stat = path.stat()
    except OSError:
        return (str(path), 0, 0)
    return (str(path), stat.st_mtime_ns, stat.st_size)


def _inbox_signature(repo_root: Path) -> tuple[tuple[str, int, int], ...]:
    rows: list[tuple[str, int, int]] = []
    for item in collect_inbox_items(repo_root):
        try:
            stat = item.source_path.stat()
        except OSError:
            continue
        rows.append((item.name, stat.st_mtime_ns, stat.st_size))
    return tuple(rows)


def serve_metrics_text(state: Any) -> str:
    bound = 0
    rollout_present = 0
    pending = 0
    for target in state.worktree_targets():
        thread_id = identity.resolve_thread_id_for_target(state, target) or ""
        if thread_id:
            bound = 1
            transcript = resolve_thread_transcript(thread_id, target.repo_root)
            if transcript is not None and transcript.path.is_file():
                rollout_present = 1
        pending += pending_inbox_count(target.repo_root)
    lines = [
        "# HELP spice_serve_bound Whether any serve target has a bound thread id.",
        "# TYPE spice_serve_bound gauge",
        f"spice_serve_bound {bound}",
        "# HELP spice_serve_pending_inbox_items Pending inbox items for serve worktrees.",
        "# TYPE spice_serve_pending_inbox_items gauge",
        f"spice_serve_pending_inbox_items {pending}",
        "# HELP spice_serve_rollout_present Whether a bound rollout file is readable.",
        "# TYPE spice_serve_rollout_present gauge",
        f"spice_serve_rollout_present {rollout_present}",
        "# HELP spice_serve_http_requests_total HTTP requests handled by this serve process.",
        "# TYPE spice_serve_http_requests_total counter",
    ]
    for (method, path), count in sorted(state.http_requests_snapshot().items()):
        labels = (
            f'method="{_prometheus_label_value(method)}",'
            f'path="{_prometheus_label_value(path)}"'
        )
        lines.append(f"spice_serve_http_requests_total{{{labels}}} {count}")
    return "\n".join(lines) + "\n"


def observer_metrics_text(state: Any) -> str:
    session_count = len(state.observer.sessions) if state.observer is not None else 0
    lines = [
        "# HELP spice_watch_sessions Number of read-only transcript sessions.",
        "# TYPE spice_watch_sessions gauge",
        f"spice_watch_sessions {session_count}",
        "# HELP spice_serve_http_requests_total HTTP requests handled by this serve process.",
        "# TYPE spice_serve_http_requests_total counter",
    ]
    for (method, path), count in sorted(state.http_requests_snapshot().items()):
        labels = (
            f'method="{_prometheus_label_value(method)}",'
            f'path="{_prometheus_label_value(path)}"'
        )
        lines.append(f"spice_serve_http_requests_total{{{labels}}} {count}")
    return "\n".join(lines) + "\n"


def serve_metrics_path_template(path: str) -> str:
    parsed = urlparse(path)
    route_path = parsed.path or "/"
    if route_path in {
        "/",
        "/metrics",
        "/api/live/bus",
        "/api/metrics/tasks/burndown",
        "/api/metrics/tasks/distribution",
        "/api/work/trees",
        "/api/teams",
        "/api/teams/command",
    }:
        return route_path
    if _team_metrics_api_route(route_path) is not None:
        return "/api/teams/{id}/metrics"
    if route_path.startswith(STATIC_ASSET_ROUTE_PREFIX):
        return "/static/{asset}"
    route = _work_tree_api_route(route_path)
    if route is not None:
        action = route[1]
        if action not in WORK_TREE_API_METRIC_ACTIONS:
            return "/api/work/trees/{id}/other"
        return "/api/work/trees/{id}" + (f"/{action}" if action else "")
    if route_path == "/work/tree" or route_path.startswith("/work/tree/"):
        return "/work/tree/{target}"
    return "other"


def _prometheus_label_value(value: str) -> str:
    return value.replace("\\", "\\\\").replace("\n", "\\n").replace('"', '\\"')


def _is_client_disconnect(exc: BaseException) -> bool:
    if isinstance(exc, ConnectionError):
        return True
    return isinstance(exc, OSError) and exc.errno in _CLIENT_DISCONNECT_ERRNOS


def _request_reader_timed_out(reader: object) -> bool:
    if not isinstance(reader, BufferedReader):
        return False
    raw = reader.raw
    return isinstance(raw, SocketIO) and bool(getattr(raw, "_timeout_occurred", False))


def _send_missing_worktree_image(handler: Any) -> None:
    handler._send_bytes(MISSING_IMAGE_PLACEHOLDER_SVG, "image/svg+xml; charset=utf-8")


def _work_tree_api_route(path: str) -> tuple[str, str] | None:
    prefix = "/api/work/trees/"
    if not path.startswith(prefix):
        return None
    remainder = path.removeprefix(prefix)
    if "/" not in remainder:
        return (remainder, "")
    target_id, action = remainder.split("/", 1)
    return (target_id, action)


def _team_metrics_api_route(path: str) -> str | None:
    prefix = "/api/teams/"
    if not path.startswith(prefix):
        return None
    remainder = path.removeprefix(prefix)
    if "/" not in remainder:
        return None
    team_id, action = remainder.split("/", 1)
    if action != "metrics" or not team_id:
        return None
    return unquote(team_id)


def work_tree_proxy_target_from_request(
    state: Any,
    parsed: Any,
) -> tuple[WorktreeTarget | None, str | None]:
    target = _work_tree_path_target_from_request(parsed)
    if target is None:
        return None, None
    selector, separator, remainder = target.partition("/")
    if not selector and separator:
        return None, f"/{remainder}"
    worktree = resolve_worktree_for_request(state, selector)
    if worktree is not None and separator:
        return worktree, remainder
    if worktree is not None:
        return worktree, ""
    return None, target


def _work_tree_path_target_from_request(parsed: Any) -> str | None:
    if parsed.path.startswith("/work/tree/"):
        target = unquote(parsed.path.removeprefix("/work/tree/"))
        return target or None
    return None


def resolve_work_tree_link_path(
    state: Any,
    target: str,
    worktree: WorktreeTarget | None,
) -> Path | None:
    parsed = urlparse(target)
    if parsed.scheme and parsed.scheme != "file":
        return None
    raw_path = parsed.path if parsed.scheme == "file" else target
    candidate = Path(raw_path).expanduser()
    roots = _work_tree_link_roots(state, worktree)
    if candidate.is_absolute():
        return _existing_allowed_path(candidate, roots)
    for root in roots:
        resolved = (root / candidate).resolve()
        if resolved.exists() and resolved.is_relative_to(root.resolve()):
            return resolved
    return None


def _work_tree_link_roots(state: Any, worktree: WorktreeTarget | None) -> list[Path]:
    roots: list[Path] = []
    candidates = [
        worktree.repo_root if worktree is not None else None,
        *(target.repo_root for target in state.worktree_targets()),
    ]
    for candidate in candidates:
        if candidate is None:
            continue
        resolved = candidate.resolve()
        if resolved not in roots:
            roots.append(resolved)
        try:
            shared = shared_attachment_root(resolved).resolve()
        except SpiceError:
            continue
        if shared not in roots:
            roots.append(shared)
    return roots


def _existing_allowed_path(candidate: Path, roots: list[Path]) -> Path | None:
    try:
        resolved = candidate.resolve()
    except OSError:
        return None
    if not resolved.exists():
        return None
    if any(resolved.is_relative_to(root) for root in roots):
        return resolved
    return None


def _directory_listing(path: Path) -> str:
    try:
        rows = sorted(
            child.name + ("/" if child.is_dir() else "") for child in path.iterdir()
        )
    except OSError:
        return ""
    return "\n".join(rows) + ("\n" if rows else "")


def _query_int(
    query: dict[str, list[str]], key: str, default: int, *, minimum: int = 1
) -> int:
    raw = query.get(key, [""])[0]
    try:
        value = int(raw)
    except ValueError:
        return default
    return value if value >= minimum else default


def _query_float(
    query: dict[str, list[str]],
    key: str,
    default: float | None,
    *,
    minimum: float = 0.0,
) -> float | None:
    raw = query.get(key, [""])[0]
    try:
        value = float(raw)
    except (TypeError, ValueError):
        return default
    return value if value >= minimum else default


def _query_finite_float(
    query: dict[str, list[str]],
    key: str,
    default: float | None,
    *,
    minimum: float = 0.0,
) -> float | None:
    value = _query_float(query, key, default, minimum=minimum)
    if value is None:
        return default
    return value if math.isfinite(value) else default


def _query_strict_finite_float(
    query: dict[str, list[str]],
    key: str,
    *,
    minimum: float = 0.0,
) -> float | None:
    raw = query.get(key, [""])[0].strip()
    if not raw:
        return None
    try:
        value = float(raw)
    except ValueError as exc:
        raise SpiceError(f"{key} must be a finite number") from exc
    if not math.isfinite(value):
        raise SpiceError(f"{key} must be a finite number")
    if value < minimum:
        raise SpiceError(f"{key} must be at least {minimum:g}")
    return value


def _query_str(query: dict[str, list[str]], key: str) -> str | None:
    raw = query.get(key, [""])[0].strip()
    return raw or None


def _query_values(query: dict[str, list[str]], key: str) -> tuple[str, ...]:
    return tuple(
        dict.fromkeys(value.strip() for value in query.get(key, []) if value.strip())
    )


def _metric_bucket_count_for_range(
    start: float, end: float, bucket_seconds: int
) -> int:
    start_bucket = metric_bucket_start(start, bucket_seconds)
    end_bucket = metric_bucket_start(end, bucket_seconds)
    if end_bucket < start_bucket:
        return 1
    return ((end_bucket - start_bucket) // bucket_seconds) + 1


def _resolve_worktree_image_path(repo_root: Path, raw: str) -> Path | None:
    root = repo_root.resolve()
    shared = resolve_shared_attachment_ref(raw, repo_root=root)
    if shared is not None:
        return shared
    candidate = Path(raw)
    resolved = (candidate if candidate.is_absolute() else root / candidate).resolve()
    for path in _worktree_image_path_candidates(root, resolved):
        if path.is_file():
            return path
    return None


def _worktree_image_path_candidates(root: Path, resolved: Path) -> tuple[Path, ...]:
    shared_candidate = resolve_shared_attachment_ref(str(resolved), repo_root=root)
    if shared_candidate is not None:
        return (shared_candidate,)
    if resolved.is_relative_to(root):
        return (resolved,)
    return ()
