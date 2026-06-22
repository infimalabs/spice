"""`spice agent run` — the command surface every agent shell command enters.

Shell startup hooks reexec zsh/bash commands through
`spice agent run -- <cmd…>`. The command surface:

* gives direct `git` invocations the agent git-shadow environment, and gives
  nested `spice` invocations a scrubbed one (harness internals must see real
  upstream config);
* connects to the supervisor side-channel socket (when one is live) and
  relays its payload to stderr;
* injects pending inbox steering into stderr, re-displaying every 15s until ACK;
* injects keep-working guidance derived from the agent's own transcript,
  repeated every 15 minutes and persisted across wrapper processes.

The agent's terminal is therefore a duplex steering surface: every command it
runs is an opportunity for the operator to be heard.
"""

from __future__ import annotations

import contextlib
import json
import os
import select
import shlex
import socket
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from threading import Thread
from typing import Any, TextIO

from spice.agent.driver import driver_for
from spice.agent.sidechannelnotify import (
    active_agent_side_channel_socket_path,
    consume_side_channel_notices,
    side_channel_marker_path as side_channel_marker_path,
)
from spice.agent.gitshadow import (
    agent_git_shadow_environment,
    scrub_agent_git_shadow_environment,
)
from spice.agent.identity import ambient_thread_id
from spice.agent.paths import agent_state_dir
from spice.agent.shellhook import SHELL_HOOK_REEXEC_STAGE_ENV
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

PYTHON_ROUTE_COMMANDS = frozenset(("python", "python3"))
SHELL_EXECUTION_COMMANDS = frozenset(("bash", "dash", "sh", "zsh"))
SHELL_EXECUTION_FLAGS = frozenset(("-c", "-lc"))
RTK_REWRITE_COMMAND = ("rtk", "rewrite")
# RTK prints a rewritten command and returns 3 from the hook path on this lane.
RTK_REWRITE_MATCH_EXIT_CODES = frozenset((0, 3))
PYTHON_ROUTE_FAILURE = (
    "import sys;"
    "sys.stderr.write("
    "'spice agent run: refusing to run python from global PATH; expected "
    "the venv interpreter at {venv_python} or an explicit interpreter path\\n'"
    ");"
    "raise SystemExit(127)"
)

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


def context_meter_cache_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-meter.json"


def context_warning_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-warning.json"


def run_agent_command(
    repo_root: Path | None,
    raw_args: Sequence[str],
    *,
    popen_factory: ProcessFactory = subprocess.Popen,
    stderr: TextIO = sys.stderr,
) -> int:
    emit_initial_side_channel_payload(repo_root, stderr=stderr)
    command = build_agent_run_command(raw_args, repo_root=repo_root, rewrite_rtk=True)
    environment = build_agent_run_environment(
        raw_args,
        repo_root=repo_root,
    )
    if environment is None:
        process = popen_factory(command)
    else:
        process = popen_factory(command, env=environment)
    watch_thread = start_agent_side_channel_watch(
        repo_root,
        parent_pid=int(getattr(process, "pid", 0) or 0),
        stderr=stderr,
        initial_payload_already_rendered=True,
    )
    try:
        wait = getattr(process, "wait", None)
        if wait is None:
            returncode = process.poll()
        else:
            returncode = wait()
        return int(returncode if returncode is not None else INTERRUPTED_EXIT_CODE)
    finally:
        join_agent_side_channel_watch(watch_thread)


def emit_initial_side_channel_payload(
    repo_root: Path | None, *, stderr: TextIO = sys.stderr
) -> None:
    if repo_root is None:
        return
    from spice.agent.sidechannel import render_side_channel_payload

    try:
        payload = render_side_channel_payload(repo_root)
    except Exception as exc:  # pragma: no cover - conflicted worktree recovery
        stderr.write(f"spice side-channel unavailable: {exc}\n")
        stderr.flush()
        return
    if payload:
        stderr.write(payload)
        stderr.flush()


def build_agent_run_command(
    raw_args: Sequence[str], *, repo_root: Path | None = None, rewrite_rtk: bool = False
) -> list[str]:
    args = normalize_agent_run_args(raw_args)
    if rewrite_rtk:
        args = rtk_rewrite_agent_run_args(args)
    routed_args = worktree_route_command(args, repo_root=repo_root)
    if rewrite_rtk and args == routed_args:
        return rtk_rewrite_direct_args(routed_args) or routed_args
    return routed_args


def rtk_rewrite_agent_run_args(args: Sequence[str]) -> list[str]:
    shell_command_index = shell_execution_command_index(args)
    if shell_command_index is None:
        return list(args)
    rewritten = rtk_rewrite_command_text(args[shell_command_index])
    if rewritten is None:
        return list(args)
    result = list(args)
    result[shell_command_index] = rewritten
    return result


def rtk_rewrite_direct_args(args: Sequence[str]) -> list[str] | None:
    if (
        not args
        or args[:1] == ["rtk"]
        or shell_execution_command_index(args) is not None
    ):
        return None
    rewritten = rtk_rewrite_command_text(*args)
    if rewritten is None:
        return None
    try:
        return shlex.split(rewritten)
    except ValueError:
        return None


def rtk_rewrite_command_text(*args: str) -> str | None:
    try:
        completed = subprocess.run(
            [*RTK_REWRITE_COMMAND, *args],
            stdout=subprocess.PIPE,
            text=True,
            check=False,
        )
    except FileNotFoundError:
        return None
    if completed.returncode not in RTK_REWRITE_MATCH_EXIT_CODES:
        return None
    rewritten = completed.stdout.strip()
    return rewritten or None


def shell_execution_command_index(args: Sequence[str]) -> int | None:
    if len(args) < 3 or args[1] not in SHELL_EXECUTION_FLAGS:
        return None
    if Path(args[0]).name not in SHELL_EXECUTION_COMMANDS:
        return None
    return 2


def build_agent_run_environment(
    raw_args: Sequence[str],
    *,
    repo_root: Path | None = None,
) -> dict[str, str] | None:
    args = normalize_agent_run_args(raw_args)
    worktree_env = agent_run_child_worktree_environment(args, repo_root=repo_root)
    if is_direct_git_route(args):
        return agent_git_shadow_environment(repo_root, base_env=worktree_env)
    if is_spice_route(args):
        # Harness internals must see real upstream config.
        scrubbed = scrub_agent_git_shadow_environment(os.environ)
        return agent_run_child_worktree_environment(
            args,
            repo_root=repo_root,
            base_env=scrubbed,
        )
    if worktree_spice_source(repo_root) is not None:
        return worktree_env
    return None


def agent_run_child_worktree_environment(
    args: Sequence[str],
    *,
    repo_root: Path | None,
    base_env: Mapping[str, str] | None = None,
) -> dict[str, str]:
    env = worktree_spice_environment(repo_root, base_env=base_env)
    if shell_execution_command_index(args) is None:
        env.pop(SHELL_HOOK_REEXEC_STAGE_ENV, None)
    else:
        env[SHELL_HOOK_REEXEC_STAGE_ENV] = "1"
    return env


def is_direct_git_route(args: Sequence[str]) -> bool:
    return args[:1] == ["git"]


def is_spice_route(args: Sequence[str]) -> bool:
    return args[:1] == ["spice"] or args[: len(_UV_RUN_SPICE)] == _UV_RUN_SPICE


def worktree_route_command(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    return worktree_python_route_command(
        worktree_spice_route_command(args, repo_root=repo_root), repo_root=repo_root
    )


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


def worktree_python_route_command(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    if args[:1] and args[0] in PYTHON_ROUTE_COMMANDS:
        return [*python_route_command_prefix(repo_root), *args[1:]]
    return list(args)


def python_route_command_prefix(repo_root: Path | None) -> list[str]:
    if worktree_spice_source(repo_root) is not None:
        return [sys.executable]
    venv_python = default_venv_python(repo_root)
    if venv_python.is_file() and os.access(venv_python, os.X_OK):
        return [str(venv_python)]
    return [
        sys.executable,
        "-c",
        PYTHON_ROUTE_FAILURE.format(venv_python=str(venv_python)),
    ]


def default_venv_python(repo_root: Path | None) -> Path:
    if "VIRTUAL_ENV" in os.environ:
        return Path(os.environ["VIRTUAL_ENV"]) / "bin" / "python"
    if repo_root is None:
        return Path(".venv") / "bin" / "python"
    return repo_root / ".venv" / "bin" / "python"


def normalize_agent_run_args(raw_args: Sequence[str]) -> list[str]:
    args = list(raw_args)
    if args[:1] == ["--"]:
        return args[1:]
    return args


def start_agent_side_channel_watch(
    repo_root: Path | None,
    *,
    parent_pid: int,
    stderr: TextIO,
    initial_payload_already_rendered: bool = False,
) -> Thread | None:
    if parent_pid <= 0 or active_agent_side_channel_socket_path(repo_root) is None:
        return None
    thread = Thread(
        target=watch_agent_side_channel,
        kwargs={
            "repo_root": repo_root,
            "parent_pid": parent_pid,
            "stderr": stderr,
            "initial_payload_already_rendered": initial_payload_already_rendered,
        },
        daemon=True,
    )
    thread.start()
    return thread


def join_agent_side_channel_watch(thread: Thread | None) -> None:
    if thread is not None:
        thread.join(timeout=1.0)


def watch_agent_side_channel(
    repo_root: Path | None,
    *,
    parent_pid: int,
    stderr: TextIO = sys.stderr,
    initial_payload_already_rendered: bool = False,
) -> None:
    socket_path = active_agent_side_channel_socket_path(repo_root)
    if socket_path is None:
        return
    side_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    parent_exit = _parent_exit_watcher(parent_pid)
    try:
        if parent_pid > 0 and parent_exit is None and not _process_exists(parent_pid):
            return
        side_socket.connect(str(socket_path))
        side_socket.sendall(
            json.dumps(
                agent_side_channel_hello(
                    repo_root,
                    runner="agent.run.watch",
                    stream_until_parent_exit=parent_pid,
                    initial_payload_already_rendered=initial_payload_already_rendered,
                ),
                separators=(",", ":"),
            ).encode("utf-8")
            + b"\n"
        )
        read_targets: list[socket.socket | _ParentExitWatcher] = [side_socket]
        if parent_exit is not None:
            read_targets.append(parent_exit)
        while True:
            readable, _, _ = select.select(read_targets, [], [])
            if parent_exit is not None and parent_exit in readable:
                return
            if side_socket not in readable:
                continue
            chunk = side_socket.recv(AGENT_RUN_SIDE_CHANNEL_READ_BYTES)
            if not chunk:
                return
            write_side_channel_chunk(stderr, chunk)
    except OSError:
        return
    finally:
        with contextlib.suppress(OSError):
            side_socket.close()
        if parent_exit is not None:
            parent_exit.close()


def agent_side_channel_hello(
    repo_root: Path | None,
    *,
    runner: str = "agent.run",
    stream_until_parent_exit: int | None = None,
    initial_payload_already_rendered: bool = False,
) -> dict[str, object]:
    hello: dict[str, object] = {
        "type": "hello",
        "pid": os.getpid(),
        "ppid": os.getppid(),
        "runner": runner,
        "cwd": os.getcwd(),
        "repoRoot": str(repo_root) if repo_root is not None else "",
    }
    if stream_until_parent_exit is not None:
        hello["streamUntilParentExit"] = stream_until_parent_exit
        hello["initialPayloadAlreadyRendered"] = initial_payload_already_rendered
    return hello


def write_side_channel_chunk(stderr: TextIO, chunk: bytes) -> None:
    buffer = getattr(stderr, "buffer", None)
    if buffer is not None:
        buffer.write(chunk)
        buffer.flush()
        return
    stderr.write(chunk.decode("utf-8", errors="replace"))
    stderr.flush()


class _ParentExitWatcher:
    def __init__(self, handle: int | select.kqueue):
        self.handle = handle

    def fileno(self) -> int:
        if isinstance(self.handle, int):
            return self.handle
        return self.handle.fileno()

    def close(self) -> None:
        if isinstance(self.handle, int):
            with contextlib.suppress(OSError):
                os.close(self.handle)
            return
        self.handle.close()


def _parent_exit_watcher(parent_pid: int) -> _ParentExitWatcher | None:
    if parent_pid <= 0:
        return None
    pidfd_open = getattr(os, "pidfd_open", None)
    if pidfd_open is not None:
        try:
            return _ParentExitWatcher(pidfd_open(parent_pid))
        except OSError:
            return None
    if hasattr(select, "kqueue") and hasattr(select, "KQ_FILTER_PROC"):
        kqueue: select.kqueue | None = None
        try:
            kqueue = select.kqueue()
            event = select.kevent(
                parent_pid,
                filter=select.KQ_FILTER_PROC,
                flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_ONESHOT,
                fflags=select.KQ_NOTE_EXIT,
            )
            kqueue.control([event], 0, 0)
            return _ParentExitWatcher(kqueue)
        except OSError:
            if kqueue is not None:
                kqueue.close()
            return None
    return None


def _process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


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
            self._emit_pending_summary(len(pending_keys))
            return
        # Always pass the recently-shown keys as the suppression filter, even
        # when a new key forced this readout: the new key renders full (real
        # time preserved) while keys still inside their window collapse to one
        # compact line each instead of re-dumping every body on every new key.
        display_filter = suppressed_keys
        try:
            from spice.mail.readout import print_inbox_readout

            displayed_keys = print_inbox_readout(
                self.repo_root,
                quiet=True,
                displayed_keys=display_filter,
                file=self.stderr,
            )
        except Exception as exc:  # pragma: no cover - conflicted worktree recovery
            self.stderr.write(f"Inbox Steering\n  unavailable={exc}\n")
            self.stderr.flush()
            displayed_keys = []
        self.stderr.flush()
        displayed_signature = inbox_pending_signature(self.repo_root)
        displayed_pending_keys = {
            inbox_key
            for inbox_key, _row_signature in _signature_rows(displayed_signature)
        }
        self.signature = displayed_signature
        self._record_displayed_keys(displayed_signature, displayed_keys, now=now)
        self._prune_display_state(displayed_pending_keys)

    def _emit_pending_summary(self, count: int) -> None:
        # Every pending item is inside its repeat-suppression window, so the full
        # readout is withheld — but emit a one-line count so a quick command never
        # *looks* empty while steering waits. The full readout returns on the next
        # repeat or via `spice session briefing`.
        if count <= 0:
            return
        self.stderr.write(
            f"Inbox Steering\n  pending={count} "
            "(recently shown; full readout on repeat or run "
            "`spice session briefing`)\n"
        )
        self.stderr.flush()

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


class AgentSideChannelNoticeInjector:
    """Write one-shot supervisor feedback to the same stderr side-channel."""

    def __init__(self, repo_root: Path | None, *, stderr: TextIO) -> None:
        self.repo_root = repo_root
        self.stderr = stderr

    def inject(self, *, force: bool) -> None:
        del force
        notices = consume_side_channel_notices(self.repo_root)
        if not notices:
            return
        self.stderr.write("Supervisor Feedback\n")
        for notice in notices:
            for line in notice.splitlines():
                self.stderr.write(f"  {line}\n")
        self.stderr.flush()


def inbox_pending_signature(repo_root: Path | None) -> InboxSignature:
    if repo_root is None:
        return ()
    directory = Path(repo_root) / STATE_DIRNAME / "inbox"
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
        (_inbox_item_key(name), (mtime_ns, size)) for name, mtime_ns, size in signature
    ]


def _inbox_item_key(name: str) -> str:
    path = Path(name)
    return path.stem or path.name


class AgentContextMeterInjector:
    """Write keep-working guidance to stderr, repeat-suppressed on disk."""

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
        transcript_path = driver_for(repo_root).thread_transcript_path(thread_id)
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
    signature = (level, snapshot.ts, snapshot.total_tokens)
    return signature, context_meter_instruction(level) + "\n"
