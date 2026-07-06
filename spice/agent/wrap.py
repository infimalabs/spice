"""`spice agent run` — the command surface every agent shell command enters.

Shell startup hooks reexec zsh/bash commands through
`spice agent run -- <cmd…>`. The command surface:

* runs routed commands in the worktree env, which inherits the per-process git
  shadow the supervisor exports once (so the agent's git upstream reads as its
  own branch); the control plane reads the real integration branch via
  `git config --get`;
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
import datetime
import json
import os
import select
import shlex
import socket
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from threading import Thread
from typing import Any, Protocol, TextIO

from spice.agent.driver import driver_for
from spice.agent.sidechannelnotify import (
    active_agent_side_channel_socket_path,
    consume_side_channel_notices,
    side_channel_marker_path as side_channel_marker_path,
)
from spice.agent.identity import ambient_thread_id
from spice.agent.paths import agent_state_dir, agent_thread_state_dir
from spice.agent.shellhook import (
    BASH_ENV_ENV,
    BASH_HOOK_NAME,
    ZDOTDIR_ENV,
    packaged_shell_steering_static_hook_dir,
)
from spice.errors import SpiceError
from spice.paths import STATE_DIRNAME
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
RTK_DB_PATH_ENV = "RTK_DB_PATH"  # env-policy: allow

AGENT_RUN_INBOX_REPEAT_SECONDS = 15.0
AGENT_RUN_CONTEXT_METER_CACHE_SECONDS = 15.0
AGENT_RUN_CONTEXT_WARNING_REPEAT_SECONDS = 15.0 * 60.0
AGENT_RUN_SIDE_CHANNEL_READ_BYTES = 8192
INTERRUPTED_EXIT_CODE = 130
COMMAND_NOT_FOUND_EXIT_CODE = 127

InboxSignature = tuple[tuple[str, int, int], ...]
ContextWarningSignature = tuple[str, str, int]
ContextWarningKey = tuple[str]
WorkingStateKey = tuple[int, str, str, int, str]


@dataclass(frozen=True)
class WorkingStateSnapshot:
    pending_inbox_count: int = 0
    claim_handle: str = ""
    claim_phase: str = ""
    claim_elapsed_seconds: int | None = None
    dirty_file_count: int = 0
    last_maxim_bag: str = ""

    def has_fields(self) -> bool:
        return bool(
            self.pending_inbox_count
            or self.claim_handle
            or self.dirty_file_count
            or self.last_maxim_bag
        )


class _KqueueHandle(Protocol):
    def fileno(self) -> int: ...

    def close(self) -> None: ...

    def control(
        self, changelist: Any, max_events: int, timeout: float | None = None
    ) -> Any: ...


def _select_has_attrs(*names: str) -> bool:
    return all(hasattr(select, name) for name in names)


def _select_attr(name: str) -> Any:
    return getattr(select, name)


ProcessFactory = Callable[..., Any]
TimeFactory = Callable[[], float]
ContextMeterFactory = Callable[[Path | None], ContextMeter | None]


def context_meter_cache_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-meter.json"


def context_warning_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-warning.json"


def working_state_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "working-state.json"


def post_tool_hook_inbox_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "post-tool-hook-inbox.json"


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
    try:
        if environment is None:
            process = popen_factory(command)
        else:
            process = popen_factory(command, env=environment)
    except FileNotFoundError:
        executable = command[0] if command else ""
        stderr.write(f"spice agent run: command not found: {executable}\n")
        stderr.flush()
        return COMMAND_NOT_FOUND_EXIT_CODE
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
    except Exception as exc:  # side-channel render failure is non-fatal
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
        args = rtk_rewrite_agent_run_args(args, repo_root=repo_root)
    routed_args = worktree_route_command(args, repo_root=repo_root)
    if rewrite_rtk and args == routed_args:
        return rtk_rewrite_direct_args(routed_args) or routed_args
    return routed_args


def rtk_rewrite_agent_run_args(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    shell_command_index = shell_execution_command_index(args)
    if shell_command_index is None:
        return list(args)
    rewritten = rtk_rewrite_shell_execution_text(
        args[shell_command_index],
        repo_root=repo_root,
    )
    if rewritten is None:
        return list(args)
    result = list(args)
    result[shell_command_index] = rewritten
    return result


def rtk_rewrite_shell_execution_text(
    command_text: str, *, repo_root: Path | None = None
) -> str | None:
    rewritten = rtk_rewrite_command_text(command_text)
    if rewritten is not None:
        return rewritten
    trailing = rtk_rewrite_trailing_exec_shell_command(command_text)
    if trailing is not None:
        return trailing
    return driver_for(repo_root).rewrite_tool_command(
        command_text, rtk_rewrite_command_text
    )


def rtk_rewrite_trailing_exec_shell_command(command_text: str) -> str | None:
    stripped = command_text.rstrip()
    trailing = command_text[len(stripped) :]
    line_start = stripped.rfind("\n") + 1
    prefix = stripped[:line_start]
    line = stripped[line_start:]
    try:
        parts = shlex.split(line)
    except ValueError:
        return None
    if len(parts) != 4 or parts[0] != "exec":
        return None
    shell, flag, nested_command = parts[1:]
    if (
        Path(shell).name not in SHELL_EXECUTION_COMMANDS
        or flag not in SHELL_EXECUTION_FLAGS
    ):
        return None
    rewritten = rtk_rewrite_command_text(nested_command)
    if rewritten is None:
        return None
    return (
        f"{prefix}exec {shlex.quote(shell)} {flag} {shlex.quote(rewritten)}{trailing}"
    )


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
            # `--` stops rtk option parsing so a flag-leading command (e.g.
            # `--help`) is rewritten as a command, not read as rtk's own option.
            [*RTK_REWRITE_COMMAND, "--", *args],
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
    # The git shadow is exported once by the supervisor (lifecycle.agent_env) and
    # inherited by direct git commands when Popen gets no explicit env. git sees
    # the shadow (upstream=self); the control plane reads the real integration
    # branch via `git config --get`, where the command-scope true merge wins over
    # the system-scope self merge.
    env = None
    if shell_execution_command_index(args) is not None:
        env = agent_run_child_worktree_environment(args, repo_root=repo_root)
    return apply_scoped_rtk_history_environment(repo_root, env)


def apply_scoped_rtk_history_environment(
    repo_root: Path | None, env: dict[str, str] | None
) -> dict[str, str] | None:
    path = scoped_rtk_history_db_path(repo_root)
    if path is None:
        return env
    path.parent.mkdir(parents=True, exist_ok=True)
    result = dict(os.environ if env is None else env)  # env-policy: allow
    result[RTK_DB_PATH_ENV] = str(path)
    return result


def scoped_rtk_history_db_path(repo_root: Path | None) -> Path | None:
    if repo_root is None:
        return None
    thread_id = ambient_thread_id()
    if not thread_id:
        return None
    try:
        return agent_thread_state_dir(repo_root, thread_id) / "rtk" / "history.db"
    except SpiceError:
        return None


def agent_run_child_worktree_environment(
    args: Sequence[str],
    *,
    repo_root: Path | None,
    base_env: Mapping[str, str] | None = None,
) -> dict[str, str]:
    env = dict(os.environ if base_env is None else base_env)  # env-policy: allow
    if shell_execution_command_index(args) is not None:
        static_hook_dir = packaged_shell_steering_static_hook_dir()
        env[ZDOTDIR_ENV] = str(static_hook_dir)
        env[BASH_ENV_ENV] = str(static_hook_dir / BASH_HOOK_NAME)
    return env


def worktree_route_command(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    del repo_root
    return worktree_python_route_command(args)


def worktree_python_route_command(args: Sequence[str]) -> list[str]:
    if args[:1] and args[0] in PYTHON_ROUTE_COMMANDS:
        return [sys.executable, *args[1:]]
    return list(args)


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
    def __init__(self, handle: int | _KqueueHandle):
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
    if _select_has_attrs(
        "kqueue",
        "kevent",
        "KQ_FILTER_PROC",
        "KQ_EV_ADD",
        "KQ_EV_ENABLE",
        "KQ_EV_ONESHOT",
        "KQ_NOTE_EXIT",
    ):
        try:
            kqueue: _KqueueHandle = _select_attr("kqueue")()
        except OSError:
            return None
        try:
            event = _select_attr("kevent")(
                parent_pid,
                filter=_select_attr("KQ_FILTER_PROC"),
                flags=(
                    _select_attr("KQ_EV_ADD")
                    | _select_attr("KQ_EV_ENABLE")
                    | _select_attr("KQ_EV_ONESHOT")
                ),
                fflags=_select_attr("KQ_NOTE_EXIT"),
            )
            kqueue.control([event], 0, 0)
            return _ParentExitWatcher(kqueue)
        except OSError:
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
    immediately. Display state is per-injector unless `state_path` is supplied
    for short-lived processes that need to share repeat-suppression state.
    """

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_INBOX_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
        state_path: Path | None = None,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.state_path = state_path
        self.displayed_at_by_key: dict[str, float] = {}
        self.displayed_signature_by_key: dict[str, tuple[int, int]] = {}
        self.signature: InboxSignature | None = None
        self._load_display_state()

    def _load_display_state(self) -> None:
        if self.state_path is None:
            return
        (
            self.displayed_at_by_key,
            self.displayed_signature_by_key,
            self.signature,
        ) = read_inbox_display_state(self.state_path)

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
        self._persist_display_state()

    def _emit_pending_summary(self, count: int) -> None:
        # Every pending item is inside its repeat-suppression window, so the full
        # readout is withheld — but emit a one-line count so a quick command never
        # *looks* empty while steering waits. The full readout returns on the next
        # repeat or via `spice session briefing`.
        if count <= 0:
            return
        from spice.mail.steeringkey import steering_token

        # Key this compact nudge exactly like the full readout, so the agent
        # never sees an unwrapped "Inbox Steering" and can always tell real
        # steering from a faked block.
        token = steering_token(self.repo_root)
        header = f"Inbox Steering  <{token}>" if token else "Inbox Steering"
        footer = f"\n  </{token}>" if token else ""
        self.stderr.write(
            f"{header}\n  pending={count} "
            "(recently shown; full readout on repeat or run "
            f"`spice session briefing`){footer}\n"
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
            age = now - last_displayed_at
            if 0 <= age < self.repeat_interval_seconds:
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

    def _persist_display_state(self) -> None:
        if self.state_path is None:
            return
        write_inbox_display_state(
            self.state_path,
            displayed_at_by_key=self.displayed_at_by_key,
            displayed_signature_by_key=self.displayed_signature_by_key,
            signature=self.signature or (),
        )


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


class AgentWorkingStateInjector:
    """Collect live working state for the one-line stderr meter."""

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_INBOX_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
        snapshot_factory: Callable[[Path | None], WorkingStateSnapshot] | None = None,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.snapshot_factory = snapshot_factory or collect_working_state_snapshot
        self.displayed_at: float | None = None
        self.displayed_key: WorkingStateKey | None = None

    def inject(self, *, force: bool) -> None:
        del force
        try:
            snapshot = self.snapshot_factory(self.repo_root)
        except Exception:
            return
        if not snapshot.has_fields():
            return
        key = working_state_key(snapshot)
        now = self.time_factory()
        if self._should_suppress(key, now=now):
            return
        text = render_working_state_snapshot(snapshot)
        if not text:
            return
        self.stderr.write(text)
        self.stderr.write("\n")
        self.stderr.flush()
        self._record_displayed(key, now=now)

    def _should_suppress(self, key: WorkingStateKey, *, now: float) -> bool:
        if self._is_recent_match(self.displayed_key, self.displayed_at, key, now=now):
            return True
        stored_key, stored_at = read_working_state_state(self.repo_root)
        if self._is_recent_match(stored_key, stored_at, key, now=now):
            self.displayed_key = stored_key
            self.displayed_at = stored_at
            return True
        return False

    def _record_displayed(self, key: WorkingStateKey, *, now: float) -> None:
        self.displayed_key = key
        self.displayed_at = now
        write_working_state_state(self.repo_root, key, now=now)

    def _is_recent_match(
        self,
        displayed_key: WorkingStateKey | None,
        displayed_at: float | None,
        key: WorkingStateKey,
        *,
        now: float,
    ) -> bool:
        if displayed_key != key or displayed_at is None:
            return False
        age = now - displayed_at
        return 0 <= age < self.repeat_interval_seconds


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


def read_inbox_display_state(
    path: Path,
) -> tuple[dict[str, float], dict[str, tuple[int, int]], InboxSignature | None]:
    payload = read_context_meter_cache_payload(path)
    raw_displayed_at = payload.get("displayedAtByKey")
    displayed_at_by_key: dict[str, float] = {}
    if isinstance(raw_displayed_at, dict):
        for key, value in raw_displayed_at.items():
            displayed_at = _float_payload_value(value)
            if isinstance(key, str) and displayed_at is not None:
                displayed_at_by_key[key] = displayed_at

    raw_displayed_signature = payload.get("displayedSignatureByKey")
    displayed_signature_by_key: dict[str, tuple[int, int]] = {}
    if isinstance(raw_displayed_signature, dict):
        for key, value in raw_displayed_signature.items():
            row_signature = _inbox_row_signature_payload(value)
            if isinstance(key, str) and row_signature is not None:
                displayed_signature_by_key[key] = row_signature

    signature = _inbox_signature_payload(payload.get("signature"))
    return displayed_at_by_key, displayed_signature_by_key, signature


def write_inbox_display_state(
    path: Path,
    *,
    displayed_at_by_key: dict[str, float],
    displayed_signature_by_key: dict[str, tuple[int, int]],
    signature: InboxSignature,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps(
            {
                "displayedAtByKey": displayed_at_by_key,
                "displayedSignatureByKey": {
                    key: list(row_signature)
                    for key, row_signature in displayed_signature_by_key.items()
                },
                "signature": [
                    [name, mtime_ns, size] for name, mtime_ns, size in signature
                ],
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def _inbox_signature_payload(value: Any) -> InboxSignature | None:
    if not isinstance(value, list):
        return None
    rows: list[tuple[str, int, int]] = []
    for row in value:
        if not isinstance(row, list) or len(row) != 3:
            return None
        name, raw_mtime_ns, raw_size = row
        mtime_ns = _int_payload_value(raw_mtime_ns)
        size = _int_payload_value(raw_size)
        if not isinstance(name, str) or mtime_ns is None or size is None:
            return None
        rows.append((name, mtime_ns, size))
    return tuple(sorted(rows))


def _inbox_row_signature_payload(value: Any) -> tuple[int, int] | None:
    if not isinstance(value, list) or len(value) != 2:
        return None
    mtime_ns = _int_payload_value(value[0])
    size = _int_payload_value(value[1])
    if mtime_ns is None or size is None:
        return None
    return (mtime_ns, size)


def _signature_rows(signature: InboxSignature) -> list[tuple[str, tuple[int, int]]]:
    return [
        (_inbox_item_key(name), (mtime_ns, size)) for name, mtime_ns, size in signature
    ]


def _inbox_item_key(name: str) -> str:
    path = Path(name)
    return path.stem or path.name


def collect_working_state_snapshot(
    repo_root: Path | None, *, now: float | None = None
) -> WorkingStateSnapshot:
    if repo_root is None:
        return WorkingStateSnapshot()
    root = Path(repo_root)
    claim_handle, claim_phase, claim_elapsed_seconds = _working_state_claim(
        root, now=now
    )
    return WorkingStateSnapshot(
        pending_inbox_count=_working_state_pending_count(root),
        claim_handle=claim_handle,
        claim_phase=claim_phase,
        claim_elapsed_seconds=claim_elapsed_seconds,
        dirty_file_count=_working_state_dirty_file_count(root),
        last_maxim_bag=_working_state_last_maxim_bag(root),
    )


def _working_state_pending_count(repo_root: Path) -> int:
    return len(inbox_pending_signature(repo_root))


def render_working_state_snapshot(snapshot: WorkingStateSnapshot) -> str:
    if not snapshot.has_fields():
        return ""
    parts: list[str] = []
    if snapshot.pending_inbox_count:
        inbox_label = (
            "pending inbox" if snapshot.pending_inbox_count == 1 else "pending inboxes"
        )
        parts.append(f"{snapshot.pending_inbox_count} {inbox_label}")
    if snapshot.claim_handle:
        claim = f"claim {_working_state_clean_text(snapshot.claim_handle)}"
        if snapshot.claim_phase:
            claim += f" {_working_state_clean_text(snapshot.claim_phase)}"
        if snapshot.claim_elapsed_seconds is not None:
            claim += f" for {_working_state_duration(snapshot.claim_elapsed_seconds)}"
        parts.append(claim)
    if snapshot.dirty_file_count:
        dirty_label = "dirty file" if snapshot.dirty_file_count == 1 else "dirty files"
        parts.append(f"{snapshot.dirty_file_count} {dirty_label}")
    if snapshot.last_maxim_bag:
        parts.append(f"last maxim {_working_state_clean_text(snapshot.last_maxim_bag)}")
    if not parts:
        return ""
    return f"🌶️ Working state: {'; '.join(parts)}."


def working_state_key(snapshot: WorkingStateSnapshot) -> WorkingStateKey:
    return (
        max(0, int(snapshot.pending_inbox_count)),
        _working_state_clean_text(snapshot.claim_handle),
        _working_state_clean_text(snapshot.claim_phase),
        max(0, int(snapshot.dirty_file_count)),
        _working_state_clean_text(snapshot.last_maxim_bag),
    )


def read_working_state_state(
    repo_root: Path | None,
) -> tuple[WorkingStateKey | None, float | None]:
    if repo_root is None:
        return None, None
    payload = read_context_meter_cache_payload(working_state_state_path(repo_root))
    key = _working_state_key_payload(payload.get("key"))
    displayed_at = _float_payload_value(payload.get("displayedAt"))
    if key is None or displayed_at is None:
        return None, None
    return key, displayed_at


def write_working_state_state(
    repo_root: Path | None, key: WorkingStateKey, *, now: float
) -> None:
    if repo_root is None:
        return
    path = working_state_state_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps({"displayedAt": now, "key": list(key)}, separators=(",", ":"))
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def _working_state_key_payload(value: Any) -> WorkingStateKey | None:
    if not isinstance(value, list) or len(value) != 5:
        return None
    pending = _int_payload_value(value[0])
    claim_handle = value[1]
    claim_phase = value[2]
    dirty = _int_payload_value(value[3])
    last_maxim = value[4]
    if (
        pending is None
        or not isinstance(claim_handle, str)
        or not isinstance(claim_phase, str)
        or dirty is None
        or not isinstance(last_maxim, str)
    ):
        return None
    return (pending, claim_handle, claim_phase, dirty, last_maxim)


def _working_state_duration(seconds: int) -> str:
    value = max(0, int(seconds))
    return f"{value}s"


def _working_state_clean_text(value: object) -> str:
    return " ".join(str(value or "").split())


def _working_state_claim(
    repo_root: Path, *, now: float | None
) -> tuple[str, str, int | None]:
    actor = ambient_thread_id()
    if not actor:
        return "", "", None
    try:
        from spice.tasks import identity, tw

        rows = tw.export(["status:pending", "+ACTIVE"])
    except Exception:
        return "", "", None
    own_rows = [
        row
        for row in rows
        if str(row.get("claim_by") or "") == actor
        and _claim_worktree_matches(row, repo_root)
    ]
    if not own_rows:
        return "", "", None
    row = max(
        own_rows,
        key=lambda item: str(item.get("claim_at") or item.get("start") or ""),
    )
    claim_started_at = _iso_timestamp_seconds(str(row.get("claim_at") or ""))
    elapsed = None
    if claim_started_at is not None:
        elapsed = int(
            max(0.0, (time.time() if now is None else now) - claim_started_at)
        )
    return identity.render_handle(row), str(row.get("phase") or ""), elapsed


def _claim_worktree_matches(row: dict[str, Any], repo_root: Path) -> bool:
    raw = str(row.get("claim_worktree") or "").strip()
    if not raw:
        return False
    try:
        return Path(raw).expanduser().resolve() == repo_root.expanduser().resolve()
    except OSError:
        return False


def _working_state_dirty_file_count(repo_root: Path) -> int:
    try:
        result = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=repo_root,
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError:
        return 0
    if result.returncode != 0:
        return 0
    return sum(1 for line in result.stdout.splitlines() if line.strip())


def _working_state_last_maxim_bag(repo_root: Path) -> str:
    try:
        from spice.agent.maximmetrics import MAXIM_EVENT_FIRE, maxim_metric_records

        records = maxim_metric_records(repo_root)
    except Exception:
        return ""
    for record in reversed(records):
        if record.event_type == MAXIM_EVENT_FIRE:
            return record.bag_name
    return ""


def _iso_timestamp_seconds(value: str) -> float | None:
    clean = str(value or "").strip()
    if not clean:
        return None
    if clean.endswith("Z"):
        clean = f"{clean[:-1]}+00:00"
    try:
        return datetime.datetime.fromisoformat(clean).timestamp()
    except ValueError:
        return None


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


def _int_payload_value(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


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
