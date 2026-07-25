"""The supervisor web app: lanes over worktrees, steering in, transcripts out."""

from __future__ import annotations

import argparse
import hmac
import ipaddress
import json
import mimetypes
import os
import subprocess
from concurrent.futures import Future
from http.cookies import CookieError, SimpleCookie
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Event, Lock
from typing import Any, Callable
from urllib.parse import parse_qs, urlparse

from spice.agent.driver import SPICE_AGENT_DRIVER_ENV, all_drivers
from spice.agent.lifecycle import agent_state_path as agent_state_path
from spice.errors import SpiceError
from spice.paths import STATE_BACKEND_TASK_DIR, repo_root_from_cwd, set_state_backend
from spice.serve.worktree import inventory
from spice.serve.payload import identity, message, metric
from spice.serve.agentapi import (
    agent_ensure_response_payload,
    agent_status_payload,
)
from spice.serve.audio import (
    normalize_say_rate_multiplier,
    render_speech_audio,
)
from spice.serve.filewatch import start_exit_file_watch
from spice.serve.launch import start_available_work_watch
from spice.serve.lifecycle import (
    AutomaticLifecycleWake,
    ExplicitLifecycleIntent,
    LifecycleOutcome,
    LifecycleReconciler,
    cancel_lifecycle_reconciler,
    join_lifecycle_reconciler,
    start_lifecycle_reconciler,
)
from spice.serve.images import rollout_image_from_offset
from spice.serve.httpapi import (
    METRICS_CONTENT_TYPE,
    METRIC_BUCKET_SECONDS as METRIC_BUCKET_SECONDS,
    STATIC_ASSET_ROUTE_PREFIX,
    TEAM_HISTORICAL_MAX_BUCKET_COUNT as TEAM_HISTORICAL_MAX_BUCKET_COUNT,
    _directory_listing,
    _is_client_disconnect,
    _query_int,
    _query_str,
    _request_reader_timed_out,
    _resolve_worktree_image_path,
    _send_missing_worktree_image,
    _team_metrics_api_route,
    _work_tree_api_route,
    lane_signature_for_target,
    lane_watch_paths_for_target,
    observer_metrics_text,
    resolve_work_tree_link_path,
    serve_metrics_path_template,
    serve_metrics_text,
    task_burndown_metrics_response_payload,
    task_distribution_metrics_response_payload,
    team_command_response_payload,
    team_historical_metrics_response_payload,
    team_snapshot_response_payload,
    work_tree_proxy_target_from_request,
)
from spice.serve.livebus import LiveBusCallbacks, serve_live_bus
from spice.serve.messages import (
    DEFAULT_MESSAGE_LIMIT,
    RolloutCursor,
    TranscriptResolution as TranscriptResolution,
    resolve_thread_transcript,
)
from spice.serve.observer import (
    ObserverRegistry,
    discover_observer_sessions,
    observer_agent_status_payload,
    observer_lane_signature,
    observer_messages_payload,
)
from spice.serve.team.store import ServeTeamStore, TeamCommandService
from spice.serve.web import render_index_html, send_static_asset
from spice.serve.websocket import is_websocket_request
from spice.serve.workroutes import (
    resolve_worktree_for_request,
    work_tree_send_accepted_response_payload,
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.worktree.bindings import reconcile_target_thread_bindings
from spice.serve.worktree.target import (
    WorktreeDiscoveryError,
    WorktreeTarget,
    discover_serve_worktrees,
)
from spice.tasks import config as task_config
from spice import defaults

DEFAULT_SERVE_HOST = defaults.string("serve", "host")
DEFAULT_SERVE_PORT = defaults.integer("serve", "port")
SERVE_UNTIL_WATCHER_JOIN_SECONDS = 1.0
SERVE_AUTH_COOKIE_NAME = "spice_serve_auth"
MAX_HTTP_REQUEST_LINE_BYTES = 65536
HTTP_REQUEST_LINE_READ_LIMIT = MAX_HTTP_REQUEST_LINE_BYTES + 1
TASK_BACKEND_LIVE_LANE_ERROR = (
    "live lane mutations are unavailable while the spice serve "
    "--task-backend override is active"
)


def _live_lane_mutation_payload(
    mutate: Callable[[], tuple[dict[str, Any], HTTPStatus]],
) -> tuple[dict[str, Any], HTTPStatus]:
    """Apply one live-lane mutation only when task state is live as well."""
    if task_config.backend_override() is not None:
        return (
            {"ok": False, "error": TASK_BACKEND_LIVE_LANE_ERROR},
            HTTPStatus.METHOD_NOT_ALLOWED,
        )
    return mutate()


def _live_bus_send_payload(
    state: ServeState, target: WorktreeTarget, payload: dict[str, Any]
) -> tuple[dict[str, Any], HTTPStatus]:
    return _live_lane_mutation_payload(
        lambda: work_tree_send_accepted_response_payload(state, target, payload)
    )


def _live_bus_task_drain_payload(
    state: ServeState, target: WorktreeTarget, payload: dict[str, Any]
) -> tuple[dict[str, Any], HTTPStatus]:
    return _live_lane_mutation_payload(
        lambda: work_tree_task_drain_response_payload(state, target, payload)
    )


class ServeState:
    # `anchor_root` is only the seed for worktree discovery: the directory
    # serve was pointed at. Nothing may branch on it being (or containing) a
    # repo; lane content, skills, and link roots come from each lane's own
    # worktree, never from where the serve process happens to live.
    def __init__(
        self,
        *,
        anchor_root: Path,
        auth_token: str | None = None,
        observer: ObserverRegistry | None = None,
        team_store: ServeTeamStore | None = None,
    ) -> None:
        if observer is not None and team_store is not None:
            raise ValueError("observer mode cannot use a team store")
        self.anchor_root = anchor_root
        self.auth_token = auth_token
        self.observer = observer
        self.cache_lock = Lock()
        self.thread_binding_lock = Lock()
        self.cached_thread_ids: dict[str, str] = {}
        self.cached_targets: list[WorktreeTarget] | None = None
        # Only discovery proves that target roots are registered worktrees with
        # lane-local git state. Tests and observer adapters may inject synthetic
        # targets into the cache; those have no thread pointers to reconcile.
        self.cached_targets_registered = False
        # `cached_targets` is cleared by every invalidate_targets() -- which the
        # live-bus targets push does before each build -- so it is empty exactly
        # when discovery fails. `last_known_targets` survives invalidation and is
        # what keeps a transient `git worktree list` failure from shipping a
        # short workTrees list that the client reads as "those lanes are gone".
        self.last_known_targets: list[WorktreeTarget] = []
        self.targets_discovery_error = ""
        self.rollout_cursors: dict[tuple[str, str], RolloutCursor] = {}
        self.pending_agent_ensure_attempts: dict[str, float] = {}
        self.http_request_counts: dict[tuple[str, str], int] = {}
        self.lifecycle_reconciler: LifecycleReconciler | None = None
        self._team_store = (
            team_store
            if team_store is not None
            else (ServeTeamStore() if observer is None else None)
        )
        self._team_commands = (
            TeamCommandService(self._team_store)
            if self._team_store is not None
            else None
        )

    @property
    def observer_mode(self) -> bool:
        return self.observer is not None

    @property
    def team_store(self) -> ServeTeamStore:
        if self._team_store is None:
            raise RuntimeError("team store is unavailable in observer mode")
        return self._team_store

    @property
    def team_commands(self) -> TeamCommandService:
        if self._team_commands is None:
            raise RuntimeError("team commands are unavailable in observer mode")
        return self._team_commands

    def submit_lifecycle_wake(
        self,
        wake: AutomaticLifecycleWake,
    ) -> Future[LifecycleOutcome]:
        reconciler = self._require_lifecycle_reconciler()
        return LifecycleReconciler.submit_automatic(reconciler, wake)

    def submit_lifecycle_intent(
        self,
        intent: ExplicitLifecycleIntent,
    ) -> Future[LifecycleOutcome]:
        reconciler = self._require_lifecycle_reconciler()
        return LifecycleReconciler.submit_intent(reconciler, intent)

    def lifecycle_outcome(self, target_id: str) -> LifecycleOutcome | None:
        reconciler = self._require_lifecycle_reconciler()
        return LifecycleReconciler.latest_outcome(reconciler, target_id)

    def _require_lifecycle_reconciler(self) -> LifecycleReconciler:
        if self.lifecycle_reconciler is None:
            raise RuntimeError("lifecycle reconciliation is unavailable")
        return self.lifecycle_reconciler

    def worktree_targets(self) -> list[WorktreeTarget]:
        if self.observer is not None:
            return self.observer.targets
        with self.cache_lock:
            if self.cached_targets is not None:
                targets = self.cached_targets
                registered = self.cached_targets_registered
            else:
                targets = None
                registered = False
        if targets is not None:
            return (
                self._reconcile_target_thread_bindings(targets)
                if registered
                else targets
            )
        try:
            targets = discover_serve_worktrees(
                cwd=self.anchor_root, fallback_roots=[self.anchor_root]
            )
        except WorktreeDiscoveryError as exc:
            # Hold the last list we actually observed and report the failure;
            # do not cache it, so the next build retries discovery.
            with self.cache_lock:
                self.targets_discovery_error = str(exc)
                targets = list(self.last_known_targets)
            return self._reconcile_target_thread_bindings(targets)
        with self.cache_lock:
            self.targets_discovery_error = ""
            self.last_known_targets = list(targets)
            if self.cached_targets is None:
                self.cached_targets = targets
                self.cached_targets_registered = True
            cached_targets = self.cached_targets
        return self._reconcile_target_thread_bindings(cached_targets)

    def _reconcile_target_thread_bindings(
        self, targets: list[WorktreeTarget]
    ) -> list[WorktreeTarget]:
        with self.thread_binding_lock:
            reconcile_target_thread_bindings(targets)
        return targets

    def targets_discovery_errors(self) -> list[str]:
        with self.cache_lock:
            return (
                [self.targets_discovery_error] if self.targets_discovery_error else []
            )

    def invalidate_targets(self) -> None:
        with self.cache_lock:
            self.cached_targets = None
            self.cached_targets_registered = False

    def record_http_request(self, method: str, path: str) -> None:
        key = (method.upper(), serve_metrics_path_template(path))
        with self.cache_lock:
            self.http_request_counts[key] = self.http_request_counts.get(key, 0) + 1

    def http_requests_snapshot(self) -> dict[tuple[str, str], int]:
        with self.cache_lock:
            return dict(self.http_request_counts)

    def rollout_cursor(self, client_id: str, thread_id: str) -> RolloutCursor:
        # Cursors are per (client, thread): each connected client tracks its own
        # stream position and removed-key delta. A single per-thread cursor let
        # one tab/machine advance past messages another had not yet seen.
        key = (client_id, thread_id)
        with self.cache_lock:
            cursor = self.rollout_cursors.get(key)
            if cursor is None:
                cursor = RolloutCursor()
                self.rollout_cursors[key] = cursor
            return cursor

    def drop_client_cursors(self, client_id: str) -> None:
        # Release a disconnected client's cursors so the per-client store does
        # not grow without bound across reconnects.
        with self.cache_lock:
            stale = [key for key in self.rollout_cursors if key[0] == client_id]
            for key in stale:
                del self.rollout_cursors[key]


def apply_serve_backends(args: argparse.Namespace) -> None:
    """Apply --backend / --task-backend scratch overrides before state resolves.

    --backend redirects every managed-state root, task store included, under
    one scratch path. An explicit --task-backend wins for the task store alone
    because the specific override is applied after the total one.
    """
    backend = getattr(args, "backend", None)
    if backend is not None:
        path = Path(backend).expanduser()
        if not path.is_absolute():
            raise SpiceError("spice serve --backend requires an absolute scratch path")
        set_state_backend(str(path))
        task_config.set_backend(str(path / STATE_BACKEND_TASK_DIR))
    task_backend = getattr(args, "task_backend", None)
    if task_backend is not None:
        path = Path(task_backend).expanduser()
        if not path.is_absolute():
            raise SpiceError(
                "spice serve --task-backend requires an absolute scratch path"
            )
        task_config.set_backend(str(path))


def run_serve(args: argparse.Namespace) -> int:
    # The operator server is never an agent and never a single-driver lane; a
    # leaked ambient thread id or driver override would make every worktree
    # inherit process-local agent state instead of its own config.
    for driver in all_drivers():
        os.environ.pop(driver.thread_id_env, None)  # env-policy: allow
    os.environ.pop(SPICE_AGENT_DRIVER_ENV, None)  # env-policy: allow
    apply_serve_backends(args)
    auth_token = serve_auth_token(args)
    guard_exposed_bind(
        args.host,
        args.port,
        allow_insecure=bool(getattr(args, "allow_insecure_bind", False)),
        auth_token=auth_token,
    )
    anchor_root = repo_root_from_cwd() or Path.cwd()
    observer = None
    if bool(getattr(args, "observer_mode", False)):
        observer = discover_observer_sessions(list(args.session_dirs))
        for error in observer.errors:
            print(f"spice watch: {error}")
    state = ServeState(
        anchor_root=anchor_root,
        auth_token=auth_token,
        observer=observer,
    )
    server = _ServeHttpServer((args.host, args.port), _ServeHandler, state)
    watch_stop = Event()
    watch_thread = start_exit_file_watch(server, args, stop_event=watch_stop)
    lifecycle_reconciler = start_lifecycle_reconciler(state)
    available_work_watch = start_available_work_watch(state)
    bound_host, bound_port = server.server_address[:2]
    host = str(bound_host)
    port = int(bound_port)
    print(f"spice serve: http://{host}:{port}")
    _warn_exposed_bind(host, port, auth_token=auth_token)
    print(f"spice serve: anchor={anchor_root}")
    if observer is not None:
        print(f"spice watch: sessions={len(observer.sessions)} read_only=true")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nspice serve: interrupted")
    finally:
        watch_stop.set()
        # Cancel before closing the server so the watcher leaves its blocking
        # wait while the socket teardown runs, rather than after it.
        if available_work_watch is not None:
            available_work_watch.cancel()
        if lifecycle_reconciler is not None:
            cancel_lifecycle_reconciler(lifecycle_reconciler)
        server.server_close()
        if watch_thread is not None:
            watch_thread.join(timeout=SERVE_UNTIL_WATCHER_JOIN_SECONDS)
        if available_work_watch is not None:
            available_work_watch.join()
        if lifecycle_reconciler is not None:
            join_lifecycle_reconciler(lifecycle_reconciler)
    return 0


def serve_auth_token(args: argparse.Namespace) -> str | None:
    raw_token = getattr(args, "auth_token", None)
    if raw_token is None:
        return None
    token = str(raw_token).strip()
    if not token:
        raise SpiceError("spice serve --auth-token requires a non-empty token")
    return token


def guard_exposed_bind(
    host: str,
    port: int,
    *,
    allow_insecure: bool,
    auth_token: str | None,
) -> None:
    if not _is_exposed_bind_host(host):
        return
    if allow_insecure or auth_token:
        return
    address = serve_address(host, port)
    raise SpiceError(
        "use --allow-insecure-bind to expose the no-auth control surface "
        "deliberately or --auth-token TOKEN to require a token; spice serve "
        f"refuses to bind it to exposed address {address}. On wildcard binds, "
        "WebSocket Origin checks degrade to Origin-equals-Host, so the token "
        "is the operative defense."
    )


def _warn_exposed_bind(host: str, port: int, *, auth_token: str | None) -> None:
    if not _is_exposed_bind_host(host):
        return
    address = serve_address(host, port)
    if auth_token:
        print(
            f"WARNING: spice serve is exposed on {address} with token auth enabled; "
            "on wildcard binds the token is the operative WebSocket defense"
        )
        return
    print(
        "WARNING: spice serve is exposing a no-auth control surface on "
        f"{address} because --allow-insecure-bind was supplied; on wildcard "
        "binds WebSocket Origin checks degrade to Origin-equals-Host with no "
        "token defense"
    )


def _is_exposed_bind_host(host: str) -> bool:
    candidate = (host or "").strip()
    if not candidate:
        return True
    if candidate.lower() == "localhost":
        return False
    try:
        return not ipaddress.ip_address(candidate).is_loopback
    except ValueError:
        return True


def serve_address(host: str, port: int) -> str:
    display_host = host or "0.0.0.0"
    try:
        address = ipaddress.ip_address(display_host)
    except ValueError:
        address = None
    if address is not None and address.version == 6:
        display_host = f"[{display_host}]"
    return f"http://{display_host}:{port}"


def _token_matches(candidate: str | None, expected: str) -> bool:
    return candidate is not None and hmac.compare_digest(
        candidate.encode("utf-8"), expected.encode("utf-8")
    )


def _send_auth_cookie_if_needed(handler: Any) -> None:
    token = getattr(handler, "_serve_auth_cookie_token", None)
    if not token:
        return
    cookie = SimpleCookie()
    cookie[SERVE_AUTH_COOKIE_NAME] = token
    cookie[SERVE_AUTH_COOKIE_NAME]["path"] = "/"
    cookie[SERVE_AUTH_COOKIE_NAME]["httponly"] = True
    cookie[SERVE_AUTH_COOKIE_NAME]["samesite"] = "Strict"
    handler.send_header("Set-Cookie", cookie.output(header="").strip())


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
        if not self._authorize_request(parsed):
            return
        if parsed.path == "/api/live/bus":
            self._serve_live_bus()
            return
        if parsed.path == "/metrics":
            self._send_text(
                observer_metrics_text(self.state)
                if self.state.observer_mode
                else serve_metrics_text(self.state),
                METRICS_CONTENT_TYPE,
            )
            return
        if parsed.path == "/":
            fast_mode = (
                False
                if self.state.observer_mode
                else self.state.team_store.global_fast_mode_enabled()
            )
            self._send_html(
                render_index_html(
                    None if self.state.observer_mode else self.state.anchor_root,
                    initial_global_settings={
                        "fastMode": fast_mode,
                        "observerMode": self.state.observer_mode,
                    },
                ),
            )
            return
        if parsed.path.startswith(STATIC_ASSET_ROUTE_PREFIX):
            send_static_asset(
                self,
                parsed.path.removeprefix(STATIC_ASSET_ROUTE_PREFIX),
                if_none_match=self.headers.get("If-None-Match"),
            )
            return
        if parsed.path == "/work/tree" or parsed.path.startswith("/work/tree/"):
            if self.state.observer_mode:
                self.send_error(
                    HTTPStatus.NOT_FOUND,
                    "work tree files are unavailable in observer mode",
                )
                return
            self._send_work_tree_path(parsed)
            return
        if parsed.path == "/api/work/trees":
            self.state.invalidate_targets()
            self._send_json(
                self.state.observer.targets_payload()
                if self.state.observer is not None
                else inventory.work_trees_payload(self.state)
            )
            return
        if parsed.path == "/api/teams":
            self._send_json(
                self.state.observer.team_snapshot_payload()
                if self.state.observer is not None
                else team_snapshot_response_payload(self.state, since_revision=None)
            )
            return
        if parsed.path == "/api/metrics/tasks/burndown":
            if self.state.observer_mode:
                self._send_observer_metrics_unavailable()
                return
            self._get_task_burndown_metrics(parsed.query)
            return
        if parsed.path == "/api/metrics/tasks/distribution":
            if self.state.observer_mode:
                self._send_observer_metrics_unavailable()
                return
            self._get_task_distribution_metrics(parsed.query)
            return
        team_metrics_team_id = _team_metrics_api_route(parsed.path)
        if team_metrics_team_id is not None:
            if self.state.observer_mode:
                self._send_observer_metrics_unavailable()
                return
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
        if not self._authorize_request(parsed):
            return
        if self.state.observer_mode:
            self._send_json(
                {"ok": False, "error": "spice watch is read-only"},
                HTTPStatus.METHOD_NOT_ALLOWED,
            )
            return
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

    def _send_observer_metrics_unavailable(self) -> None:
        self._send_json(
            {"ok": False, "error": "metrics are unavailable in observer mode"},
            HTTPStatus.NOT_FOUND,
        )

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

    def _get_task_distribution_metrics(self, query_string: str) -> None:
        try:
            payload = task_distribution_metrics_response_payload(
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
                observer_messages_payload(
                    self.state,
                    target,
                    limit=_query_int(query, "limit", DEFAULT_MESSAGE_LIMIT),
                    after=_query_str(query, "after"),
                    before=_query_str(query, "before"),
                )
                if self.state.observer_mode
                else message.messages_payload_for_worktree(
                    self.state,
                    target,
                    limit=_query_int(query, "limit", DEFAULT_MESSAGE_LIMIT),
                    after=_query_str(query, "after"),
                    before=_query_str(query, "before"),
                    expected_thread_id=_query_str(query, "threadId"),
                )
            )
            return
        if action == "agent/status":
            self._send_json(
                observer_agent_status_payload(
                    self.state.observer.session_for_target(target)
                )
                if self.state.observer is not None
                else agent_status_payload(target)
            )
            return
        if action == "messages/image":
            self._send_message_image(target, query)
            return
        if action == "files/image":
            if self.state.observer_mode:
                self.send_error(
                    HTTPStatus.NOT_FOUND,
                    "work tree files are unavailable in observer mode",
                )
                return
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
        if self.state.observer is not None:
            transcript = self.state.observer.session_for_target(target).transcript
        else:
            thread_id = identity.resolve_thread_id_for_target(self.state, target)
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
            if _query_str(query, "missing") == "placeholder":
                _send_missing_worktree_image(self)
                return
            self.send_error(HTTPStatus.NOT_FOUND, "image not found in work tree")
            return
        content_type, _encoding = mimetypes.guess_type(resolved.name)
        if not content_type or not content_type.startswith("image/"):
            if _query_str(query, "missing") == "placeholder":
                _send_missing_worktree_image(self)
                return
            self.send_error(HTTPStatus.NOT_FOUND, "not an image file")
            return
        try:
            data = resolved.read_bytes()
        except OSError:
            if _query_str(query, "missing") == "placeholder":
                _send_missing_worktree_image(self)
                return
            self.send_error(HTTPStatus.NOT_FOUND, "image not found in work tree")
            return
        self._send_bytes(data, content_type)

    def _send_work_tree_path(self, parsed: Any) -> None:
        worktree, target = work_tree_proxy_target_from_request(self.state, parsed)
        if target is None:
            self.send_error(HTTPStatus.BAD_REQUEST, "target is required")
            return
        path = resolve_work_tree_link_path(self.state, target, worktree)
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
            request_payload = self._read_payload()
            payload, status = _live_lane_mutation_payload(
                lambda: work_tree_send_response_payload(
                    self.state, target, request_payload
                )
            )
            self._send_json(payload, status)
            return
        if action == "agent/ensure":
            self._read_payload()
            payload, status = _live_lane_mutation_payload(
                lambda: agent_ensure_response_payload(
                    target,
                    fast_mode=bool(self.state.team_store.global_fast_mode_enabled()),
                )
            )
            self._send_json(payload, status)
            return
        if action == "say":
            # Speech renders request text only; it does not steer, spawn, or wake a lane.
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
            audio = render_speech_audio(
                text, repo_root=target.repo_root, rate_multiplier=rate
            )
        except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
            self._send_json(
                {"ok": False, "error": f"Could not render speech audio: {exc}"},
                HTTPStatus.INTERNAL_SERVER_ERROR,
            )
            return
        self._send_bytes(audio.data, audio.content_type)

    # ---- live bus --------------------------------------------------------

    def _serve_live_bus(self) -> None:
        if not is_websocket_request(self):
            self.send_error(HTTPStatus.BAD_REQUEST, "WebSocket upgrade required")
            return
        state = self.state
        if state.observer is not None:
            observer = state.observer
            serve_live_bus(
                self,
                LiveBusCallbacks(
                    resolve_target=observer.match,
                    work_trees_payload=observer.targets_payload,
                    messages_payload=lambda target, **kwargs: observer_messages_payload(
                        state, target, **kwargs
                    ),
                    send_payload=lambda _target, _payload: (
                        {"ok": False, "error": "spice watch is read-only"},
                        HTTPStatus.METHOD_NOT_ALLOWED,
                    ),
                    task_drain_payload=lambda _target, _payload: (
                        {"ok": False, "error": "spice watch is read-only"},
                        HTTPStatus.METHOD_NOT_ALLOWED,
                    ),
                    team_snapshot_payload=lambda _revision: (
                        observer.team_snapshot_payload()
                    ),
                    team_command_payload=lambda _payload: (
                        {"ok": False, "error": "spice watch is read-only"},
                        HTTPStatus.METHOD_NOT_ALLOWED,
                    ),
                    metric_series_payload=lambda _query: {
                        "ok": False,
                        "error": "metrics are unavailable in observer mode",
                    },
                    lane_metrics_payload=lambda _target: {},
                    thread_id=lambda target: (
                        observer.session_for_target(target).thread_id
                    ),
                    transcript_resolution=observer.transcript_for_thread,
                    lane_watch_paths=lambda target, _thread_id, _transcript: (
                        observer.session_for_target(target).transcript.path,
                    ),
                    lane_signature=lambda target, _thread_id, _transcript: (
                        observer_lane_signature(observer.session_for_target(target))
                    ),
                    drop_client_cursors=lambda client_id: state.drop_client_cursors(
                        client_id
                    ),
                ),
            )
            return
        serve_live_bus(
            self,
            LiveBusCallbacks(
                resolve_target=lambda selector: resolve_worktree_for_request(
                    state, selector
                ),
                work_trees_payload=lambda: (
                    state.invalidate_targets() or inventory.work_trees_payload(state)
                ),
                messages_payload=lambda target, **kwargs: (
                    message.messages_payload_for_worktree(state, target, **kwargs)
                ),
                send_payload=lambda target, payload: _live_bus_send_payload(
                    state, target, payload
                ),
                task_drain_payload=lambda target, payload: _live_bus_task_drain_payload(
                    state, target, payload
                ),
                team_snapshot_payload=lambda since_revision: (
                    team_snapshot_response_payload(state, since_revision=since_revision)
                ),
                team_command_payload=lambda payload: team_command_response_payload(
                    state, payload
                ),
                metric_series_payload=lambda query: metric.metric_series_payload(
                    state, query
                ),
                lane_metrics_payload=lambda target: (
                    message.lane_metrics_summary_payload(state, target)
                ),
                thread_id=lambda target: identity.resolve_thread_id_for_target(
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
                send_followup_payload=lambda target, payload: (
                    message.messages_payload_for_worktree(
                        state,
                        target,
                        limit=DEFAULT_MESSAGE_LIMIT,
                    )
                ),
                drop_client_cursors=lambda client_id: state.drop_client_cursors(
                    client_id
                ),
            ),
        )

    # ---- plumbing --------------------------------------------------------

    def _authorize_request(self, parsed: Any) -> bool:
        required_token = self.state.auth_token
        if required_token is None:
            return True
        query_token = _query_str(parse_qs(parsed.query), "token")
        if _token_matches(query_token, required_token):
            self._serve_auth_cookie_token = required_token
            return True
        if _token_matches(self._bearer_auth_token(), required_token):
            return True
        if _token_matches(self.headers.get("X-Spice-Serve-Token"), required_token):
            return True
        if _token_matches(self._cookie_auth_token(), required_token):
            return True
        self._send_auth_required()
        return False

    def _bearer_auth_token(self) -> str | None:
        authorization = self.headers.get("Authorization") or ""
        scheme, separator, value = authorization.partition(" ")
        if separator and scheme.lower() == "bearer":
            return value.strip()
        return None

    def _cookie_auth_token(self) -> str | None:
        raw_cookie = self.headers.get("Cookie") or ""
        if not raw_cookie:
            return None
        cookie = SimpleCookie()
        try:
            cookie.load(raw_cookie)
        except CookieError:
            return None
        morsel = cookie.get(SERVE_AUTH_COOKIE_NAME)
        return morsel.value if morsel is not None else None

    def _send_auth_required(self) -> None:
        body = b"spice serve auth token required\n"
        self.send_response(HTTPStatus.UNAUTHORIZED)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("WWW-Authenticate", 'Bearer realm="spice serve"')
        self.end_headers()
        self.wfile.write(body)

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
        _send_auth_cookie_if_needed(self)
        self.end_headers()
        self.wfile.write(body)

    def _send_text(
        self, text: str, content_type: str = "text/plain; charset=utf-8"
    ) -> None:
        body = text.encode("utf-8")
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        _send_auth_cookie_if_needed(self)
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
        _send_auth_cookie_if_needed(self)
        self.end_headers()
        self.wfile.write(body)

    def _send_bytes(self, data: bytes, content_type: str) -> None:
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        _send_auth_cookie_if_needed(self)
        self.end_headers()
        self.wfile.write(data)
