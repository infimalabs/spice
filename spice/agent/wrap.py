"""`spice agent run` — the wrapper every agent shell command goes through.

`spice.sh` execs `spice agent run -- <cmd…>`. The wrapper:

* routes the command through a token-optimizing proxy (`rtk` by default,
  `SPICE_PROXY_BIN` to override, plain exec when the proxy is not installed);
* gives direct `git` invocations the agent git-shadow environment, and gives
  nested `spice` invocations a scrubbed one (harness internals must see real
  upstream config);
* connects to the supervisor side-channel socket (when one is live) and
  relays its payload to stderr;
* injects pending inbox steering into stderr, re-displaying every 15s until ACK;
* injects context-pressure warnings derived from the agent's own transcript,
  repeated every 15 minutes and persisted across wrapper processes.

The agent's terminal is therefore a duplex steering surface: every command it
runs is an opportunity for the operator to be heard.
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any, TextIO

from spice.agent.driver import DRIVER
from spice.agent.gitshadow import (
    agent_git_shadow_environment,
    scrub_agent_git_shadow_environment,
)
from spice.agent.identity import ambient_thread_id
from spice.mail.inbox import inbox_dir, inbox_item_key
from spice.paths import (
    STATE_DIRNAME,
    worktree_spice_environment,
    worktree_spice_python_command,
    worktree_spice_source,
)
from spice.sessions.meter import (
    ContextMeter,
    active_context_percent,
    collect_latest_context_meter,
    context_meter_cache_payload,
    context_meter_from_cache_payload,
    context_meter_instruction,
    context_pressure_level,
    context_pressure_should_warn,
)
from spice.sessions.util import format_float, format_int

PROXY_BIN_ENV = "SPICE_PROXY_BIN"  # env-policy: allow
DEFAULT_PROXY_BIN = "rtk"

AGENT_RUN_INBOX_REPEAT_SECONDS = 15.0
AGENT_RUN_CONTEXT_METER_CACHE_SECONDS = 15.0
AGENT_RUN_CONTEXT_WARNING_REPEAT_SECONDS = 15.0 * 60.0
AGENT_RUN_SIDE_CHANNEL_READ_BYTES = 8192
INTERRUPTED_EXIT_CODE = 130
_UV_RUN_SPICE = ["uv", "run", "spice"]

InboxSignature = tuple[tuple[str, int, int], ...]
ContextWarningSignature = tuple[str, str, int]
ContextWarningKey = tuple[str]
ProcessFactory = Callable[..., Any]
TimeFactory = Callable[[], float]
ContextMeterFactory = Callable[[Path | None], ContextMeter | None]


def agent_state_dir(repo_root: Path) -> Path:
    return repo_root / STATE_DIRNAME / "agents" / DRIVER.state_dirname


def context_meter_cache_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-meter.json"


def context_warning_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-warning.json"


def side_channel_marker_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "side-channel" / "socket"


def proxy_bin() -> str:
    return os.environ.get(PROXY_BIN_ENV, DEFAULT_PROXY_BIN)


def run_agent_command(
    repo_root: Path | None,
    raw_args: Sequence[str],
    *,
    popen_factory: ProcessFactory = subprocess.Popen,
    stderr: TextIO = sys.stderr,
) -> int:
    command = build_agent_run_command(raw_args, repo_root=repo_root)
    environment = build_agent_run_environment(raw_args, repo_root=repo_root)
    inject_agent_side_channel(repo_root, stderr=stderr)
    if environment is None:
        process = popen_factory(command)
    else:
        process = popen_factory(command, env=environment)
    wait = getattr(process, "wait", None)
    if wait is None:
        returncode = process.poll()
    else:
        returncode = wait()
    return int(returncode if returncode is not None else INTERRUPTED_EXIT_CODE)


def build_agent_run_command(
    raw_args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    args = normalize_agent_run_args(raw_args)
    spice_route = is_spice_route(args)
    routed_args = worktree_spice_route_command(args, repo_root=repo_root)
    proxy = proxy_bin()
    resolved_proxy = shutil.which(proxy)
    if resolved_proxy is None:
        # No proxy installed: drop the explicit `proxy` verb and run the
        # command exactly as given. The injection channels still apply.
        return routed_args[1:] if routed_args[:1] == ["proxy"] else routed_args
    if args[:1] == ["proxy"]:
        return [resolved_proxy, "proxy", *routed_args[1:]]
    if spice_route:
        return [resolved_proxy, "proxy", *routed_args]
    if requires_native_find_semantics(args):
        return routed_args
    return [resolved_proxy, *routed_args]


def build_agent_run_environment(
    raw_args: Sequence[str], *, repo_root: Path | None = None
) -> dict[str, str] | None:
    args = normalize_agent_run_args(raw_args)
    worktree_env = worktree_spice_environment(repo_root)
    if is_direct_git_route(args):
        return agent_git_shadow_environment(repo_root, base_env=worktree_env)
    if is_spice_route(args):
        # Harness internals must see real upstream config.
        scrubbed = scrub_agent_git_shadow_environment(os.environ)
        return worktree_spice_environment(repo_root, base_env=scrubbed)
    if worktree_spice_source(repo_root) is not None:
        return worktree_env
    return None


def is_direct_git_route(args: Sequence[str]) -> bool:
    return args[:1] == ["git"] or args[:2] == ["proxy", "git"]


def is_spice_route(args: Sequence[str]) -> bool:
    return args[:1] == ["spice"] or args[: len(_UV_RUN_SPICE)] == _UV_RUN_SPICE


def worktree_spice_route_command(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    if args[:1] == ["spice"]:
        return worktree_spice_python_command(repo_root, list(args[1:])) or list(args)
    if args[: len(_UV_RUN_SPICE)] == _UV_RUN_SPICE:
        return worktree_spice_python_command(
            repo_root, list(args[len(_UV_RUN_SPICE) :])
        ) or list(args)
    return list(args)


def normalize_agent_run_args(raw_args: Sequence[str]) -> list[str]:
    args = list(raw_args)
    if args[:1] == ["--"]:
        return args[1:]
    return args


def requires_native_find_semantics(args: Sequence[str]) -> bool:
    return args[:1] == ["find"]


def inject_agent_side_channel(repo_root: Path | None, *, stderr: TextIO) -> None:
    socket_path = active_agent_side_channel_socket_path(repo_root)
    if socket_path is None:
        return
    hello = agent_side_channel_hello(repo_root)
    side_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        side_socket.connect(str(socket_path))
        side_socket.sendall(
            json.dumps(hello, separators=(",", ":")).encode("utf-8") + b"\n"
        )
        while True:
            chunk = side_socket.recv(AGENT_RUN_SIDE_CHANNEL_READ_BYTES)
            if not chunk:
                return
            write_side_channel_chunk(stderr, chunk)
    except OSError:
        return
    finally:
        with contextlib.suppress(OSError):
            side_socket.close()


def active_agent_side_channel_socket_path(repo_root: Path | None) -> Path | None:
    if repo_root is None:
        return None
    marker_path = side_channel_marker_path(repo_root)
    try:
        raw_socket_path = marker_path.read_text(encoding="utf-8").strip()
    except OSError:
        return None
    if not raw_socket_path:
        return None
    return Path(raw_socket_path)


def agent_side_channel_hello(repo_root: Path | None) -> dict[str, object]:
    return {
        "type": "hello",
        "pid": os.getpid(),
        "ppid": os.getppid(),
        "runner": "agent.run",
        "cwd": os.getcwd(),
        "repoRoot": str(repo_root) if repo_root is not None else "",
    }


def write_side_channel_chunk(stderr: TextIO, chunk: bytes) -> None:
    buffer = getattr(stderr, "buffer", None)
    if buffer is not None:
        buffer.write(chunk)
        buffer.flush()
        return
    stderr.write(chunk.decode("utf-8", errors="replace"))
    stderr.flush()


class AgentInboxInjector:
    """Re-display pending inbox steering on the agent's stderr until it is ACK'd.

    Each pending item re-displays every `repeat_interval_seconds`; an item
    whose bytes changed (new signature) or that is brand new shows
    immediately. Display state is per-injector — the supervisor side-channel
    builds one per payload, the wrapper keeps one per process.
    """

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_INBOX_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.displayed_at_by_key: dict[str, float] = {}
        self.displayed_signature_by_key: dict[str, tuple[int, int]] = {}
        self.signature: InboxSignature | None = None

    def inject(self, *, force: bool) -> None:
        signature = inbox_pending_signature(self.repo_root)
        now = self.time_factory()
        suppressed_keys = self._suppressed_keys(signature, now=now)
        pending_keys = {
            inbox_key for inbox_key, _row_signature in _signature_rows(signature)
        }
        previous_pending_keys = {
            inbox_key
            for inbox_key, _row_signature in _signature_rows(self.signature or ())
        }
        new_pending_keys = pending_keys - previous_pending_keys
        if (
            not force
            and not new_pending_keys
            and signature == self.signature
            and pending_keys <= suppressed_keys
        ):
            return
        self.signature = signature
        display_filter = set[str]() if new_pending_keys else suppressed_keys
        from spice.mail.readout import print_inbox_readout

        with contextlib.redirect_stdout(self.stderr):
            displayed_keys = print_inbox_readout(
                self.repo_root,
                quiet=True,
                displayed_keys=display_filter,
            )
        self._record_displayed_keys(signature, displayed_keys, now=now)
        self._prune_display_state(pending_keys)

    def _suppressed_keys(self, signature: InboxSignature, *, now: float) -> set[str]:
        suppressed: set[str] = set()
        for key, row_signature in _signature_rows(signature):
            if self.displayed_signature_by_key.get(key) != row_signature:
                continue
            last_displayed_at = self.displayed_at_by_key.get(key)
            if last_displayed_at is None:
                continue
            if now - last_displayed_at < self.repeat_interval_seconds:
                suppressed.add(key)
        return suppressed

    def _record_displayed_keys(
        self, signature: InboxSignature, displayed_keys: list[str], *, now: float
    ) -> None:
        signature_by_key = dict(_signature_rows(signature))
        for key in displayed_keys:
            row_signature = signature_by_key.get(key)
            if row_signature is None:
                continue
            self.displayed_at_by_key[key] = now
            self.displayed_signature_by_key[key] = row_signature

    def _prune_display_state(self, pending_keys: set[str]) -> None:
        for key in list(self.displayed_at_by_key):
            if key not in pending_keys:
                self.displayed_at_by_key.pop(key, None)
                self.displayed_signature_by_key.pop(key, None)


def inbox_pending_signature(repo_root: Path | None) -> InboxSignature:
    if repo_root is None:
        return ()
    directory = inbox_dir(repo_root)
    rows: list[tuple[str, int, int]] = []
    try:
        with os.scandir(directory) as entries:
            for entry in entries:
                try:
                    if not entry.is_file() or not entry.name.endswith(".txt"):
                        continue
                    stat_result = entry.stat()
                except OSError:
                    continue
                rows.append((entry.name, stat_result.st_mtime_ns, stat_result.st_size))
    except OSError:
        return ()
    return tuple(sorted(rows))


def _signature_rows(signature: InboxSignature) -> list[tuple[str, tuple[int, int]]]:
    return [
        (inbox_item_key(name), (mtime_ns, size)) for name, mtime_ns, size in signature
    ]


class AgentContextMeterInjector:
    """Write context-pressure warnings to stderr, repeat-suppressed on disk."""

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_INBOX_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
        meter_factory: ContextMeterFactory,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.meter_factory = meter_factory
        self.displayed_at: float | None = None
        self.displayed_key: ContextWarningKey | None = None

    def inject(self, *, force: bool) -> None:
        warning = render_agent_context_warning(self.meter_factory(self.repo_root))
        if warning is None:
            return
        signature, text = warning
        key = context_warning_key(signature)
        now = self.time_factory()
        if self._should_suppress(key, now=now):
            return
        self.stderr.write(text)
        if not text.endswith("\n"):
            self.stderr.write("\n")
        self.stderr.flush()
        self._record_displayed(key, now=now)

    def _should_suppress(self, key: ContextWarningKey, *, now: float) -> bool:
        if self._is_recent_match(self.displayed_key, self.displayed_at, key, now=now):
            return True
        stored_key, stored_at = read_context_warning_state(self.repo_root)
        if self._is_recent_match(stored_key, stored_at, key, now=now):
            self.displayed_key = stored_key
            self.displayed_at = stored_at
            return True
        return False

    def _record_displayed(self, key: ContextWarningKey, *, now: float) -> None:
        self.displayed_key = key
        self.displayed_at = now
        write_context_warning_state(self.repo_root, key, now=now)

    def _is_recent_match(
        self,
        displayed_key: ContextWarningKey | None,
        displayed_at: float | None,
        key: ContextWarningKey,
        *,
        now: float,
    ) -> bool:
        if displayed_key != key or displayed_at is None:
            return False
        age = now - displayed_at
        return 0 <= age < self.repeat_interval_seconds


def agent_context_meter(repo_root: Path | None) -> ContextMeter | None:
    thread_id = ambient_thread_id()
    if repo_root is None or not thread_id:
        return None
    now = time.time()
    cached = read_cached_agent_context_meter(repo_root, thread_id, now=now)
    if cached is not None:
        return cached
    try:
        transcript_path = DRIVER.thread_transcript_path(thread_id)
    except (RuntimeError, SystemExit):
        return None
    try:
        meter = collect_latest_context_meter([transcript_path])
    except OSError:
        return None
    write_cached_agent_context_meter(repo_root, thread_id, meter, now=now)
    return meter


def context_warning_key(signature: ContextWarningSignature) -> ContextWarningKey:
    return (signature[0],)


def read_context_warning_state(
    repo_root: Path | None,
) -> tuple[ContextWarningKey | None, float | None]:
    if repo_root is None:
        return None, None
    payload = read_context_meter_cache_payload(context_warning_state_path(repo_root))
    raw_key = payload.get("key")
    displayed_at = _float_payload_value(payload.get("displayedAt"))
    if (
        not isinstance(raw_key, list)
        or len(raw_key) != 1
        or not isinstance(raw_key[0], str)
        or displayed_at is None
    ):
        return None, None
    return (raw_key[0],), displayed_at


def write_context_warning_state(
    repo_root: Path | None, key: ContextWarningKey, *, now: float
) -> None:
    if repo_root is None:
        return
    path = context_warning_state_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps({"displayedAt": now, "key": list(key)}, separators=(",", ":"))
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def read_cached_agent_context_meter(
    repo_root: Path, thread_id: str, *, now: float
) -> ContextMeter | None:
    payload = read_context_meter_cache_payload(context_meter_cache_path(repo_root))
    if payload.get("threadId") != thread_id:
        return None
    checked_at = _float_payload_value(payload.get("checkedAt"))
    if checked_at is None:
        return None
    if now - checked_at > AGENT_RUN_CONTEXT_METER_CACHE_SECONDS:
        return None
    return context_meter_from_cache_payload(payload.get("meter"))


def read_context_meter_cache_payload(path: Path) -> dict[str, Any]:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def write_cached_agent_context_meter(
    repo_root: Path, thread_id: str, meter: ContextMeter, *, now: float
) -> None:
    path = context_meter_cache_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps(
            {
                "checkedAt": now,
                "threadId": thread_id,
                "meter": context_meter_cache_payload(meter),
            },
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def _float_payload_value(value: Any) -> float | None:
    return float(value) if isinstance(value, int | float) else None


def render_agent_context_warning(
    meter: ContextMeter | None,
) -> tuple[ContextWarningSignature, str] | None:
    if meter is None or meter.latest_snapshot is None:
        return None
    snapshot = meter.latest_snapshot
    percent = active_context_percent(snapshot)
    level = context_pressure_level(percent)
    if not context_pressure_should_warn(level):
        return None
    window = snapshot.model_context_window or 0
    signature = (level, snapshot.ts, snapshot.total_tokens)
    lines = [
        "Context Pressure",
        (
            f"  level={level} active_context={format_int(snapshot.total_tokens)}/"
            f"{format_int(window)} ({format_float(percent)}%)"
        ),
        f"  keep_working={context_meter_instruction(level)}",
    ]
    return signature, "\n".join(lines) + "\n"
