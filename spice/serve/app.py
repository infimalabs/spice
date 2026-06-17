"""The supervisor web app: lanes over worktrees, steering in, transcripts out."""

from __future__ import annotations

import argparse
import errno
import json
import mimetypes
import os
import subprocess
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Event, Lock
from typing import Any
from urllib.parse import parse_qs, unquote, urlparse

from spice.agent.driver import ALL_DRIVERS, SPICE_AGENT_DRIVER_ENV
from spice.agent.lifecycle import agent_status
from spice.agent.renewal import renewal_handoff_request_text, renewal_steering_text
from spice.errors import SpiceError
from spice.mail.attachments import INBOX_ATTACHMENT_DIR_SUFFIX
from spice.mail.inbox import (
    INBOX_ARCHIVE_DIRNAME,
    collect_inbox_items,
    inbox_dir,
    pending_inbox_count,
)
from spice.paths import repo_root_from_cwd
from spice.serve import payloads
from spice.serve.agentapi import (
    agent_ensure_response_payload,
    agent_status_payload,
    sent_steering_response_payload,
)
from spice.serve.audio import (
    SAY_AUDIO_CONTENT_TYPE,
    normalize_say_rate_multiplier,
    render_say_audio,
)
from spice.serve.drive import drive_drain_queue_controls
from spice.serve.filewatch import start_exit_file_watch
from spice.serve.images import rollout_image_from_offset
from spice.serve.livebus import LiveBusCallbacks, serve_live_bus
from spice.serve.messages import (
    DEFAULT_MESSAGE_LIMIT,
    RolloutCursor,
    transcript_path_for_thread,
)
from spice.serve.steering import steering_submit_error_status, submit_steering_message
from spice.serve.teams import ServeTeamStore, TeamCommandService, TeamConfig
from spice.serve.web import render_index_html, send_static_asset
from spice.serve.websocket import is_websocket_request
from spice.serve.worktrees import (
    WorktreeTarget,
    discover_serve_worktrees,
    match_serve_worktree,
)
from spice.tasks import config as task_config

DEFAULT_SERVE_HOST = "127.0.0.1"
DEFAULT_SERVE_PORT = 8765
STATIC_ASSET_ROUTE_PREFIX = "/static/"
LIFETIME_LABELS = ("Steer", "Drive", "Drain")
SERVE_UNTIL_WATCHER_JOIN_SECONDS = 1.0
METRICS_CONTENT_TYPE = "text/plain; version=0.0.4; charset=utf-8"
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
        self.lane_send_counts: dict[str, int] = {}
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

    def record_lane_send(self, target_id: str, *, agent_id: str = "") -> None:
        with self.cache_lock:
            count = self.lane_send_counts.get(target_id, 0)
            self.lane_send_counts[target_id] = count + 1
        if agent_id:
            self.team_store.record_agent_metric_delta(agent_id, sends=1)

    def lane_send_count(self, target_id: str) -> int:
        with self.cache_lock:
            return self.lane_send_counts.get(target_id, 0)

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


def resolve_worktree_for_request(
    state: ServeState, selector: str | None
) -> WorktreeTarget | None:
    return match_serve_worktree(state.worktree_targets(), selector)


def work_tree_send_response_payload(
    state: ServeState,
    target: WorktreeTarget,
    payload: dict[str, Any],
) -> tuple[dict[str, Any], HTTPStatus]:
    text = str(payload.get("text") or "").strip()
    lifetime = str(payload.get("lifetime") or "").strip()
    drive_agent = lifetime in {"Drive", "Drain"}
    fast_mode = bool(payload.get("fastMode"))
    if not text:
        return {
            "ok": False,
            "error": "Message text is required.",
        }, HTTPStatus.BAD_REQUEST
    predecessor = payloads.resolve_thread_id_for_target(state, target) or ""
    renew_intent = bool(
        predecessor and state.team_store.agent_renewal_active(predecessor)
    )
    _apply_lifetime_to_team(state, target, payload)
    force_new = False
    if renew_intent:
        status = agent_status(target.repo_root)
        if not predecessor:
            return (
                {
                    "ok": False,
                    "error": "Could not renew agent: missing target thread id",
                },
                HTTPStatus.CONFLICT,
            )
        if status.running:
            # Renew never yanks a running agent; the message asks for a clean
            # handoff and the successor starts on the next send.
            text = renewal_handoff_request_text(text)
            try:
                state.team_store.record_pending_renewal(
                    agent_id=predecessor, ancestor_thread_id=predecessor
                )
            except SpiceError:
                pass  # renewal bookkeeping requires a team; steering still lands
        else:
            force_new = True
            text = renewal_steering_text(text, previous_thread_id=predecessor)
    try:
        sent = submit_steering_message(
            text=text,
            priority=None,
            stop=False,
            no_say=bool(payload.get("noSay")),
            attachments=payload.get("attachments"),
            controls=drive_drain_queue_controls(drive_agent),
            target_repo_root=target.repo_root,
        )
    except (RuntimeError, ValueError) as exc:
        return {"ok": False, "error": str(exc)}, steering_submit_error_status(exc)
    response_payload = sent_steering_response_payload(
        sent,
        state=state,
        target=target,
        fast_mode=fast_mode,
        force_new=force_new,
    )
    agent_ensure = response_payload.get("agentEnsure")
    if renew_intent and force_new:
        ensured_thread_id = payloads.record_started_renewal_from_ensure(
            state.team_store,
            predecessor_agent_id=predecessor,
            agent_ensure=agent_ensure if isinstance(agent_ensure, dict) else None,
        )
    else:
        ensured_thread_id = payloads.agent_ensure_thread_id(
            agent_ensure if isinstance(agent_ensure, dict) else None
        )
    send_agent_id = (
        ensured_thread_id or payloads.resolve_thread_id_for_target(state, target) or ""
    )
    state.record_lane_send(target.id, agent_id=send_agent_id)
    renewal_agent_id = predecessor if renew_intent else send_agent_id
    if renewal_agent_id:
        response_payload["renewalIntent"] = payloads.renewal_intent_for_actor(
            state.team_store, renewal_agent_id
        )
    return response_payload, HTTPStatus.OK


def _apply_lifetime_to_team(
    state: ServeState, target: WorktreeTarget, payload: dict[str, Any]
) -> None:
    lifetime = str(payload.get("lifetime") or "").strip()
    if lifetime not in LIFETIME_LABELS:
        return
    actor = payloads.resolve_thread_id_for_target(state, target) or ""
    if not actor:
        return
    team_id = state.team_store.current_team_for_agent(actor)
    if team_id is None:
        return
    current = state.team_store.team_config(team_id)
    if current.lifetime == lifetime:
        return
    state.team_store.update_team_config(
        team_id,
        TeamConfig(
            lifetime=lifetime,
            speech_mode=current.speech_mode,
            task_filters=current.task_filters,
            selected_view=current.selected_view,
            shell_settings=current.shell_settings,
        ),
        replace_task_filters=False,
    )


def work_tree_task_drain_response_payload(
    state: ServeState,
    target: WorktreeTarget,
    payload: dict[str, Any],
) -> tuple[dict[str, Any], HTTPStatus]:
    _apply_lifetime_to_team(state, target, payload)
    task_filters = payload.get("taskFilters")
    if bool(payload.get("replaceTaskFilters")) and isinstance(task_filters, list):
        actor = payloads.resolve_thread_id_for_target(state, target) or ""
        if not actor:
            return (
                {"ok": False, "error": "task drain requires a bound agent"},
                HTTPStatus.CONFLICT,
            )
        team_id = state.team_store.current_team_for_agent(actor)
        if team_id is None:
            created = state.team_store.create_team(members=[actor])
            team_id = created.team_id
        current = state.team_store.team_config(team_id)
        from spice.tasks import config as task_config

        validated = tuple(
            task_config.validate_assignable_project(str(item))
            for item in task_filters
            if str(item or "").strip()
        )
        state.team_store.update_team_config(
            team_id,
            TeamConfig(
                lifetime=str(payload.get("lifetime") or current.lifetime),
                speech_mode=current.speech_mode,
                task_filters=validated,
                selected_view=current.selected_view,
                shell_settings=current.shell_settings,
            ),
            replace_task_filters=True,
        )
    actor = payloads.resolve_thread_id_for_target(state, target) or ""
    facts = payloads.team_facts_for_actor(state.team_store, actor)
    route = {
        "actor": actor,
        "teamId": facts.get("teamId", ""),
        "teamRevision": facts.get("teamRevision", 0),
        "configRevision": facts.get("configRevision", 0),
        "memberAgents": [actor] if actor else [],
        "laneName": target.name,
        "taskFilters": facts.get("taskFilters", []),
        "taskFilterEntries": facts.get("taskFilterEntries", []),
        "routeFilters": facts.get("taskFilters", []),
        "filterTerms": facts.get("taskFilters", []),
        "filterArgs": facts.get("taskFilters", []),
        "laneFilterVersion": "",
        "lifetime": facts.get("lifetime", ""),
    }
    return {"ok": True, "route": route}, HTTPStatus.OK


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
        result = state.team_commands.apply(payload)
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


def lane_watch_paths_for_target(
    state: ServeState,
    target: WorktreeTarget,
    thread_id: str | None,
    transcript_path: Path | None,
) -> tuple[Path, ...]:
    del thread_id
    target_inbox = inbox_dir(target.repo_root)
    team_path = state.team_store.path
    paths = [target_inbox, target_inbox.parent, team_path, team_path.parent]
    paths.extend(_task_backend_watch_paths())
    if transcript_path is not None:
        paths.append(transcript_path)
    return tuple(paths)


def lane_signature_for_target(
    state: ServeState,
    target: WorktreeTarget,
    thread_id: str | None,
    transcript_path: Path | None,
) -> tuple[Any, ...]:
    team_facts = payloads.team_facts_for_actor(state.team_store, thread_id or "")
    return (
        _path_signature(transcript_path),
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
        _task_backend_signature(),
    )


def _path_signature(path: Path | None) -> tuple[str, int, int]:
    if path is None:
        return ("", 0, 0)
    try:
        stat = path.stat()
    except OSError:
        return (str(path), 0, 0)
    return (str(path), stat.st_mtime_ns, stat.st_size)


def _task_backend_watch_paths() -> tuple[Path, ...]:
    try:
        root = task_config.backend_root()
        data = task_config.data_dir()
        taskrc = task_config.taskrc_path()
    except SpiceError:
        return ()
    paths: list[Path] = [root, data, taskrc]
    try:
        paths.extend(sorted(data.iterdir(), key=lambda path: str(path)))
    except OSError:
        pass
    return tuple(paths)


def _task_backend_signature() -> tuple[tuple[str, int, int], ...]:
    return tuple(_path_signature(path) for path in _task_backend_watch_paths())


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
        thread_id = payloads.resolve_thread_id_for_target(state, target) or ""
        if thread_id:
            bound = 1
            transcript_path = transcript_path_for_thread(thread_id, target.repo_root)
            if transcript_path is not None and transcript_path.is_file():
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
        "/api/work/trees",
        "/api/teams",
        "/api/teams/command",
    }:
        return route_path
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
            self._send_html(render_index_html())
            return
        if parsed.path.startswith(STATIC_ASSET_ROUTE_PREFIX):
            send_static_asset(self, parsed.path.removeprefix(STATIC_ASSET_ROUTE_PREFIX))
            return
        if parsed.path == "/work/tree" or parsed.path.startswith("/work/tree/"):
            self._send_work_tree_path(parsed)
            return
        if parsed.path == "/api/work/trees":
            self.state.invalidate_targets()
            self._send_json(payloads.work_trees_payload(self.state))
            return
        if parsed.path == "/api/teams":
            self._send_json(
                team_snapshot_response_payload(self.state, since_revision=None)
            )
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

    def _get_work_tree(self, route: tuple[str, str], query_string: str) -> None:
        target = resolve_worktree_for_request(self.state, route[0])
        if target is None:
            self.send_error(HTTPStatus.NOT_FOUND, "work tree not found")
            return
        action = route[1]
        query = parse_qs(query_string)
        if action == "messages":
            self._send_json(
                payloads.messages_payload_for_worktree(
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
                payloads.ack_context_payload_for_worktree(
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
        thread_id = payloads.resolve_thread_id_for_target(self.state, target)
        path = (
            transcript_path_for_thread(thread_id, target.repo_root)
            if thread_id
            else None
        )
        if path is None:
            self.send_error(HTTPStatus.NOT_FOUND, "target thread is not bound")
            return
        result = rollout_image_from_offset(path, offset=offset, item_index=item)
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
                    state.invalidate_targets() or payloads.work_trees_payload(state)
                ),
                messages_payload=lambda target, **kwargs: (
                    payloads.messages_payload_for_worktree(state, target, **kwargs)
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
                thread_id=lambda target: payloads.resolve_thread_id_for_target(
                    state, target
                ),
                transcript_path=transcript_path_for_thread,
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


def _query_str(query: dict[str, list[str]], key: str) -> str | None:
    raw = query.get(key, [""])[0].strip()
    return raw or None


def _resolve_worktree_image_path(repo_root: Path, raw: str) -> Path | None:
    root = repo_root.resolve()
    candidate = Path(raw)
    resolved = (candidate if candidate.is_absolute() else root / candidate).resolve()
    for path in _worktree_image_path_candidates(root, resolved):
        if path.is_file():
            return path
    return None


def _worktree_image_path_candidates(root: Path, resolved: Path) -> tuple[Path, ...]:
    inbox_candidates = _inbox_attachment_path_candidates(root, resolved)
    if inbox_candidates:
        return inbox_candidates
    if resolved.is_relative_to(root):
        return (resolved,)
    return ()


def _inbox_attachment_path_candidates(root: Path, resolved: Path) -> tuple[Path, ...]:
    inbox_root = inbox_dir(root).resolve()
    try:
        relative = resolved.relative_to(inbox_root)
    except ValueError:
        return ()
    parts = relative.parts
    if len(parts) < 2:
        return ()
    attachment_parts = parts[1:] if parts[0] == INBOX_ARCHIVE_DIRNAME else parts
    if not attachment_parts[0].endswith(INBOX_ATTACHMENT_DIR_SUFFIX):
        return ()
    archive = (inbox_root / INBOX_ARCHIVE_DIRNAME / Path(*attachment_parts)).resolve()
    live = (inbox_root / Path(*attachment_parts)).resolve()
    candidates: list[Path] = []
    for path in (archive, live):
        if path.is_relative_to(root) and path not in candidates:
            candidates.append(path)
    return tuple(candidates)
