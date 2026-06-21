"""The supervisor web app: lanes over worktrees, steering in, transcripts out."""

from __future__ import annotations

import argparse
import errno
import json
import math
import mimetypes
import os
import subprocess
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from io import BufferedReader
from pathlib import Path
from socket import SocketIO
from threading import Event, Lock
from typing import Any
from urllib.parse import parse_qs, unquote, urlparse

from spice.agent.driver import ALL_DRIVERS, SPICE_AGENT_DRIVER_ENV
from spice.errors import SpiceError
from spice.mail.attachments import resolve_shared_attachment_ref
from spice.mail.inbox import (
    collect_inbox_items,
    inbox_dir,
    pending_inbox_count,
)
from spice.paths import repo_root_from_cwd, shared_attachment_root
from spice.serve import identitypayload, messagepayload, metricpayload, worktreepayload
from spice.serve.agentapi import (
    agent_ensure_response_payload,
    agent_status_payload,
)
from spice.serve.audio import (
    SAY_AUDIO_CONTENT_TYPE,
    normalize_say_rate_multiplier,
    render_say_audio,
)
from spice.serve.filewatch import start_exit_file_watch
from spice.serve.images import rollout_image_from_offset
from spice.serve.livebus import LiveBusCallbacks, serve_live_bus
from spice.serve.messages import (
    DEFAULT_MESSAGE_LIMIT,
    RolloutCursor,
    TranscriptResolution,
    resolve_thread_transcript,
)
from spice.serve.teammetrics import METRIC_BUCKET_SECONDS
from spice.serve.teams import ServeTeamStore, TeamCommandService
from spice.serve.web import render_index_html, send_static_asset
from spice.serve.websocket import is_websocket_request
from spice.serve.workroutes import (
    resolve_worktree_for_request,
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.worktrees import (
    WorktreeTarget,
    discover_serve_worktrees,
)
from spice.tasks import config as task_config

DEFAULT_SERVE_HOST = "127.0.0.1"
DEFAULT_SERVE_PORT = 8765
STATIC_ASSET_ROUTE_PREFIX = "/static/"
SERVE_UNTIL_WATCHER_JOIN_SECONDS = 1.0
METRICS_CONTENT_TYPE = "text/plain; version=0.0.4; charset=utf-8"
MAX_HTTP_REQUEST_LINE_BYTES = 65536
HTTP_REQUEST_LINE_READ_LIMIT = MAX_HTTP_REQUEST_LINE_BYTES + 1
TEAM_HISTORICAL_METRIC_BUCKET_COUNT = 12
TASK_BURNDOWN_BUCKET_COUNT = 12
TASK_BURNDOWN_MAX_BUCKET_COUNT = 1440
WORK_TREE_API_METRIC_ACTIONS = frozenset(
    {
        "",
        "acks",
        "agent/ensure",
        "agent/status",
        "files/image",
        "messages",
        "messages/image",
        "say",
        "send",
    }
)

_CLIENT_DISCONNECT_ERRNOS = frozenset(
    {errno.EBADF, errno.ECONNRESET, errno.EPIPE, errno.ECONNABORTED}
)


class ServeState:
    # `anchor_root` is only the seed for worktree discovery: the directory
    # serve was pointed at. Nothing may branch on it being (or containing) a
    # repo; lane content, skills, and link roots come from each lane's own
    # worktree, never from where the serve process happens to live.
    def __init__(self, *, anchor_root: Path) -> None:
        self.anchor_root = anchor_root
        self.cache_lock = Lock()
        self.cached_thread_ids: dict[str, str] = {}
        self.cached_targets: list[WorktreeTarget] | None = None
        self.rollout_cursors: dict[str, RolloutCursor] = {}
        self.pending_agent_ensure_attempts: dict[str, float] = {}
        self.http_request_counts: dict[tuple[str, str], int] = {}
        self.team_store = ServeTeamStore()
        self.team_commands = TeamCommandService(self.team_store)

    def worktree_targets(self) -> list[WorktreeTarget]:
        with self.cache_lock:
            if self.cached_targets is not None:
                return self.cached_targets
        targets = discover_serve_worktrees(
            cwd=self.anchor_root, fallback_roots=[self.anchor_root]
        )
        with self.cache_lock:
            if self.cached_targets is None:
                self.cached_targets = targets
            return self.cached_targets

    def invalidate_targets(self) -> None:
        with self.cache_lock:
            self.cached_targets = None

    def record_http_request(self, method: str, path: str) -> None:
        key = (method.upper(), serve_metrics_path_template(path))
        with self.cache_lock:
            self.http_request_counts[key] = self.http_request_counts.get(key, 0) + 1

    def http_requests_snapshot(self) -> dict[tuple[str, str], int]:
        with self.cache_lock:
            return dict(self.http_request_counts)

    def rollout_cursor(self, thread_id: str) -> RolloutCursor:
        with self.cache_lock:
            cursor = self.rollout_cursors.get(thread_id)
            if cursor is None:
                cursor = RolloutCursor()
                self.rollout_cursors[thread_id] = cursor
            return cursor


def run_serve(args: argparse.Namespace) -> int:
    # The operator server is never an agent and never a single-driver lane; a
    # leaked ambient thread id or driver override would make every worktree
    # inherit process-local agent state instead of its own config.
    for driver in ALL_DRIVERS:
        os.environ.pop(driver.thread_id_env, None)
    os.environ.pop(SPICE_AGENT_DRIVER_ENV, None)
    backend = getattr(args, "task_backend", None)
    if backend is not None:
        path = Path(backend).expanduser()
        if not path.is_absolute():
            raise SpiceError(
                "spice serve --task-backend requires an absolute scratch path"
            )
        task_config.set_backend(str(path))
    anchor_root = repo_root_from_cwd() or Path.cwd()
    state = ServeState(anchor_root=anchor_root)
    server = _ServeHttpServer((args.host, args.port), _ServeHandler, state)
    watch_stop = Event()
    watch_thread = start_exit_file_watch(server, args, stop_event=watch_stop)
    host, port = server.server_address[:2]
    print(f"spice serve: http://{host}:{port}")
    print(f"spice serve: anchor={anchor_root}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nspice serve: interrupted")
    finally:
        watch_stop.set()
        server.server_close()
        if watch_thread is not None:
            watch_thread.join(timeout=SERVE_UNTIL_WATCHER_JOIN_SECONDS)
    return 0


class _ServeHttpServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(
        self,
        server_address: tuple[str, int],
        handler_class: type[BaseHTTPRequestHandler],
        state: ServeState,
    ) -> None:
        self.spice_state = state
        super().__init__(server_address, handler_class)


def team_snapshot_response_payload(
    state: ServeState, *, since_revision: int | None
) -> dict[str, Any]:
    snapshot = state.team_store.team_snapshot(since_revision=since_revision)
    changed = since_revision is None or snapshot.global_revision > since_revision
    return {
        "ok": True,
        "revision": snapshot.global_revision,
        "changed": changed,
        "snapshot": snapshot.to_payload(),
    }


def team_command_response_payload(
    state: ServeState, payload: dict[str, Any]
) -> tuple[dict[str, Any], HTTPStatus]:
    try:
        result = state.team_commands.apply(
            identitypayload.normalize_team_command_payload(
                payload, targets=state.worktree_targets()
            )
        )
    except SpiceError as exc:
        return {"ok": False, "error": str(exc)}, HTTPStatus.CONFLICT
    return (
        {
            "ok": True,
            "revision": result.revision,
            "snapshot": result.snapshot.to_payload(),
        },
        HTTPStatus.OK,
    )


def team_historical_metrics_response_payload(
    state: ServeState,
    team_id: str,
    query: dict[str, list[str]],
) -> dict[str, Any]:
    bucket_seconds = _query_int(query, "bucketSeconds", METRIC_BUCKET_SECONDS)
    summary_time = _query_float(query, "end", None, minimum=0.0)
    if summary_time is None:
        summary_time = _query_float(query, "now", None, minimum=0.0)
    if summary_time is None:
        summary_time = time.time()
    raw_start = _query_float(query, "start", None, minimum=0.0)
    if raw_start is None:
        bucket_count = _query_int(
            query,
            "bucketCount",
            TEAM_HISTORICAL_METRIC_BUCKET_COUNT,
        )
    else:
        bucket_count = _metric_bucket_count_for_range(
            raw_start,
            summary_time,
            bucket_seconds,
        )
    summary = state.team_store.team_historical_metric_summary(
        team_id,
        bucket_count=bucket_count,
        bucket_seconds=bucket_seconds,
        now=summary_time,
    )
    window_end = _metric_bucket_start(summary_time, bucket_seconds)
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
    state: ServeState,
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
        window_end = _metric_bucket_start(end_time, bucket_seconds)
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
        window_start = _metric_bucket_start(raw_start, bucket_seconds)
        window_end = _metric_bucket_start(end_time, bucket_seconds)
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


def lane_watch_paths_for_target(
    state: ServeState,
    target: WorktreeTarget,
    thread_id: str | None,
    transcript: TranscriptResolution | None,
) -> tuple[Path, ...]:
    del thread_id
    target_inbox = inbox_dir(target.repo_root)
    target_inbox.mkdir(parents=True, exist_ok=True)
    team_path = state.team_store.path
    paths = [
        target_inbox,
        *_team_store_watch_paths(team_path),
        task_config.ensure_task_event_file(),
    ]
    if transcript is not None:
        paths.append(transcript.path)
    return tuple(paths)


def lane_signature_for_target(
    state: ServeState,
    target: WorktreeTarget,
    thread_id: str | None,
    transcript: TranscriptResolution | None,
) -> tuple[Any, ...]:
    team_facts = identitypayload.team_facts_for_target(
        state.team_store, target, thread_id
    )
    return (
        _path_signature(transcript.path if transcript else None),
        transcript.owner_driver.name if transcript else "",
        _inbox_signature(target.repo_root),
        (
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
        ),
        _path_signature(state.team_store.path),
        _path_signature(task_config.ensure_task_event_file()),
    )


def _team_store_watch_paths(path: Path) -> tuple[Path, ...]:
    return (
        path,
        path.with_name(f"{path.name}-wal"),
        path.with_name(f"{path.name}-shm"),
    )


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


def serve_metrics_text(state: ServeState) -> str:
    bound = 0
    rollout_present = 0
    pending = 0
    for target in state.worktree_targets():
        thread_id = identitypayload.resolve_thread_id_for_target(state, target) or ""
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


def serve_metrics_path_template(path: str) -> str:
    parsed = urlparse(path)
    route_path = parsed.path or "/"
    if route_path in {
        "/",
        "/metrics",
        "/api/live/bus",
        "/api/metrics/tasks/burndown",
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


class _ServeHandler(BaseHTTPRequestHandler):
    server_version = "spice-serve"
    protocol_version = "HTTP/1.1"

    def handle(self) -> None:
        try:
            super().handle()
        except OSError as exc:
            if _is_client_disconnect(exc):
                return
            raise

    def handle_one_request(self) -> None:
        try:
            try:
                self.raw_requestline = self.rfile.readline(HTTP_REQUEST_LINE_READ_LIMIT)
            except TimeoutError:
                raise
            except OSError:
                if _request_reader_timed_out(self.rfile):
                    self.close_connection = True
                    return
                raise
            if len(self.raw_requestline) > MAX_HTTP_REQUEST_LINE_BYTES:
                self.requestline = ""
                self.request_version = ""
                self.command = ""
                self.send_error(HTTPStatus.REQUEST_URI_TOO_LONG)
                return
            if not self.raw_requestline:
                self.close_connection = True
                return
            if not self.parse_request():
                return
            method_name = "do_" + self.command
            if not hasattr(self, method_name):
                self.send_error(
                    HTTPStatus.NOT_IMPLEMENTED,
                    "Unsupported method (%r)" % self.command,
                )
                return
            method = getattr(self, method_name)
            method()
            self.wfile.flush()
        except TimeoutError as exc:
            self.log_error("Request timed out: %r", exc)
            self.close_connection = True
            return

    @property
    def state(self) -> ServeState:
        return self.server.spice_state  # type: ignore[attr-defined]

    def log_message(self, format: str, *args: object) -> None:
        return

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        parsed = urlparse(self.path)
        self.state.record_http_request("GET", parsed.path)
        if parsed.path == "/api/live/bus":
            self._serve_live_bus()
            return
        if parsed.path == "/metrics":
            self._send_text(serve_metrics_text(self.state), METRICS_CONTENT_TYPE)
            return
        if parsed.path == "/":
            self._send_html(render_index_html(self.state.anchor_root))
            return
        if parsed.path.startswith(STATIC_ASSET_ROUTE_PREFIX):
            send_static_asset(self, parsed.path.removeprefix(STATIC_ASSET_ROUTE_PREFIX))
            return
        if parsed.path == "/work/tree" or parsed.path.startswith("/work/tree/"):
            self._send_work_tree_path(parsed)
            return
        if parsed.path == "/api/work/trees":
            self.state.invalidate_targets()
            self._send_json(worktreepayload.work_trees_payload(self.state))
            return
        if parsed.path == "/api/teams":
            self._send_json(
                team_snapshot_response_payload(self.state, since_revision=None)
            )
            return
        if parsed.path == "/api/metrics/tasks/burndown":
            self._get_task_burndown_metrics(parsed.query)
            return
        team_metrics_team_id = _team_metrics_api_route(parsed.path)
        if team_metrics_team_id is not None:
            self._get_team_metrics(team_metrics_team_id, parsed.query)
            return
        route = _work_tree_api_route(parsed.path)
        if route is not None:
            self._get_work_tree(route, parsed.query)
            return
        self.send_error(HTTPStatus.NOT_FOUND)

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        parsed = urlparse(self.path)
        self.state.record_http_request("POST", parsed.path)
        if parsed.path == "/api/teams/command":
            payload, status = team_command_response_payload(
                self.state, self._read_payload()
            )
            self._send_json(payload, status)
            return
        route = _work_tree_api_route(parsed.path)
        if route is not None:
            self._post_work_tree(route)
            return
        self.send_error(HTTPStatus.NOT_FOUND)

    # ---- GET routes ----------------------------------------------------

    def _get_team_metrics(self, team_id: str, query_string: str) -> None:
        try:
            payload = team_historical_metrics_response_payload(
                self.state,
                team_id,
                parse_qs(query_string),
            )
        except SpiceError as exc:
            self._send_json({"ok": False, "error": str(exc)}, HTTPStatus.BAD_REQUEST)
            return
        self._send_json(payload)

    def _get_task_burndown_metrics(self, query_string: str) -> None:
        try:
            payload = task_burndown_metrics_response_payload(
                self.state,
                parse_qs(query_string),
            )
        except SpiceError as exc:
            self._send_json({"ok": False, "error": str(exc)}, HTTPStatus.BAD_REQUEST)
            return
        self._send_json(payload)

    def _get_work_tree(self, route: tuple[str, str], query_string: str) -> None:
        target = resolve_worktree_for_request(self.state, route[0])
        if target is None:
            self.send_error(HTTPStatus.NOT_FOUND, "work tree not found")
            return
        action = route[1]
        query = parse_qs(query_string)
        if action == "messages":
            self._send_json(
                messagepayload.messages_payload_for_worktree(
                    self.state,
                    target,
                    limit=_query_int(query, "limit", DEFAULT_MESSAGE_LIMIT),
                    after=_query_str(query, "after"),
                    before=_query_str(query, "before"),
                    expected_thread_id=_query_str(query, "threadId"),
                )
            )
            return
        if action == "acks":
            self._send_json(
                messagepayload.ack_context_payload_for_worktree(
                    self.state, target, keys=query.get("key", [])
                )
            )
            return
        if action == "agent/status":
            self._send_json(agent_status_payload(target))
            return
        if action == "messages/image":
            self._send_message_image(target, query)
            return
        if action == "files/image":
            self._send_worktree_image(target, query)
            return
        self.send_error(HTTPStatus.NOT_FOUND)

    def _send_message_image(
        self, target: WorktreeTarget, query: dict[str, list[str]]
    ) -> None:
        offset = _query_int(query, "offset", -1, minimum=0)
        item = _query_int(query, "item", -1, minimum=0)
        if offset < 0 or item < 0:
            self.send_error(HTTPStatus.BAD_REQUEST, "offset and item are required")
            return
        thread_id = identitypayload.resolve_thread_id_for_target(self.state, target)
        transcript = (
            resolve_thread_transcript(thread_id, target.repo_root)
            if thread_id
            else None
        )
        if transcript is None:
            self.send_error(HTTPStatus.NOT_FOUND, "target thread is not bound")
            return
        result = rollout_image_from_offset(
            transcript.path,
            offset=offset,
            item_index=item,
            driver=transcript.owner_driver,
        )
        if result is None:
            self.send_error(HTTPStatus.NOT_FOUND, "message image not found")
            return
        image_bytes, content_type = result
        self._send_bytes(image_bytes, content_type)

    def _send_worktree_image(
        self, target: WorktreeTarget, query: dict[str, list[str]]
    ) -> None:
        raw = _query_str(query, "path") or ""
        if not raw:
            self.send_error(HTTPStatus.BAD_REQUEST, "path is required")
            return
        resolved = _resolve_worktree_image_path(target.repo_root, raw)
        if resolved is None:
            self.send_error(HTTPStatus.NOT_FOUND, "image not found in work tree")
            return
        content_type, _encoding = mimetypes.guess_type(resolved.name)
        if not content_type or not content_type.startswith("image/"):
            self.send_error(HTTPStatus.NOT_FOUND, "not an image file")
            return
        try:
            data = resolved.read_bytes()
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND, "image not found in work tree")
            return
        self._send_bytes(data, content_type)

    def _send_work_tree_path(self, parsed: Any) -> None:
        worktree, target = _work_tree_proxy_target_from_request(self.state, parsed)
        if target is None:
            self.send_error(HTTPStatus.BAD_REQUEST, "target is required")
            return
        path = _resolve_work_tree_link_path(self.state, target, worktree)
        if path is None:
            self.send_error(HTTPStatus.NOT_FOUND, "work tree path not found")
            return
        if path.is_dir():
            self._send_text(_directory_listing(path))
            return
        self._send_file(path)

    # ---- POST routes ---------------------------------------------------

    def _post_work_tree(self, route: tuple[str, str]) -> None:
        target = resolve_worktree_for_request(self.state, route[0])
        if target is None:
            self.send_error(HTTPStatus.NOT_FOUND, "work tree not found")
            return
        action = route[1]
        if action == "send":
            payload, status = work_tree_send_response_payload(
                self.state, target, self._read_payload()
            )
            self._send_json(payload, status)
            return
        if action == "agent/ensure":
            request_payload = self._read_payload()
            payload, status = agent_ensure_response_payload(
                target,
                fast_mode=bool(request_payload.get("fastMode")),
            )
            self._send_json(payload, status)
            return
        if action == "say":
            self._post_say(target)
            return
        self.send_error(HTTPStatus.NOT_FOUND)

    def _post_say(self, target: WorktreeTarget) -> None:
        payload = self._read_payload()
        text = str(payload.get("text") or "").strip()
        if not text:
            self._send_json(
                {"ok": False, "error": "Speech text is required."},
                HTTPStatus.BAD_REQUEST,
            )
            return
        rate = normalize_say_rate_multiplier(payload.get("rate"))
        try:
            audio = render_say_audio(
                text, repo_root=target.repo_root, rate_multiplier=rate
            )
        except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
            self._send_json(
                {"ok": False, "error": f"Could not render speech audio: {exc}"},
                HTTPStatus.INTERNAL_SERVER_ERROR,
            )
            return
        self._send_bytes(audio, SAY_AUDIO_CONTENT_TYPE)

    # ---- live bus --------------------------------------------------------

    def _serve_live_bus(self) -> None:
        if not is_websocket_request(self):
            self.send_error(HTTPStatus.BAD_REQUEST, "WebSocket upgrade required")
            return
        state = self.state
        serve_live_bus(
            self,
            LiveBusCallbacks(
                resolve_target=lambda selector: resolve_worktree_for_request(
                    state, selector
                ),
                work_trees_payload=lambda: (
                    state.invalidate_targets()
                    or worktreepayload.work_trees_payload(state)
                ),
                messages_payload=lambda target, **kwargs: (
                    messagepayload.messages_payload_for_worktree(
                        state, target, **kwargs
                    )
                ),
                send_payload=lambda target, payload: work_tree_send_response_payload(
                    state, target, payload
                ),
                task_drain_payload=lambda target, payload: (
                    work_tree_task_drain_response_payload(state, target, payload)
                ),
                team_snapshot_payload=lambda since_revision: (
                    team_snapshot_response_payload(state, since_revision=since_revision)
                ),
                team_command_payload=lambda payload: team_command_response_payload(
                    state, payload
                ),
                metric_series_payload=lambda query: metricpayload.metric_series_payload(
                    state, query
                ),
                thread_id=lambda target: identitypayload.resolve_thread_id_for_target(
                    state, target
                ),
                transcript_resolution=resolve_thread_transcript,
                lane_watch_paths=lambda target, thread_id, transcript_path: (
                    lane_watch_paths_for_target(
                        state, target, thread_id, transcript_path
                    )
                ),
                lane_signature=lambda target, thread_id, transcript_path: (
                    lane_signature_for_target(state, target, thread_id, transcript_path)
                ),
            ),
        )

    # ---- plumbing --------------------------------------------------------

    def _read_payload(self) -> dict[str, Any]:
        try:
            length = int(self.headers.get("Content-Length") or "0")
        except ValueError:
            return {}
        raw = self.rfile.read(length).decode("utf-8", errors="replace")
        content_type = self.headers.get("Content-Type") or ""
        if "application/json" in content_type:
            try:
                loaded = json.loads(raw or "{}")
            except json.JSONDecodeError:
                return {}
            return loaded if isinstance(loaded, dict) else {}
        form = parse_qs(raw)
        return {key: values[-1] for key, values in form.items() if values}

    def _send_html(self, html_text: str) -> None:
        body = html_text.encode("utf-8")
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_text(
        self, text: str, content_type: str = "text/plain; charset=utf-8"
    ) -> None:
        body = text.encode("utf-8")
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_file(self, path: Path) -> None:
        try:
            body = path.read_bytes()
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND, "link target not readable")
            return
        content_type = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        self._send_bytes(body, content_type)

    def _send_json(
        self, payload: dict[str, Any], status: HTTPStatus = HTTPStatus.OK
    ) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _send_bytes(self, data: bytes, content_type: str) -> None:
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


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


def _work_tree_proxy_target_from_request(
    state: ServeState,
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


def _resolve_work_tree_link_path(
    state: ServeState,
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


def _work_tree_link_roots(
    state: ServeState, worktree: WorktreeTarget | None
) -> list[Path]:
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
    start_bucket = _metric_bucket_start(start, bucket_seconds)
    end_bucket = _metric_bucket_start(end, bucket_seconds)
    if end_bucket < start_bucket:
        return 1
    return ((end_bucket - start_bucket) // bucket_seconds) + 1


def _metric_bucket_start(timestamp: float, bucket_seconds: int) -> int:
    raw = max(0, int(float(timestamp)))
    return raw - (raw % max(1, int(bucket_seconds)))


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
